use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command as StdCommand;
#[cfg(target_os = "macos")]
use std::process::Stdio;
use tokio::process::Command;

const APPROVAL_REQUIRED: &str = "native sandbox unavailable; approval required";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxPolicy {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

impl SandboxPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::FullAccess => "full-access",
        }
    }

    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("workspace-write") {
            "read-only" => Ok(Self::ReadOnly),
            "workspace-write" => Ok(Self::WorkspaceWrite),
            "full-access" => Ok(Self::FullAccess),
            other => Err(Error::Validation(format!(
                "Invalid sandbox_policy '{other}'; expected read-only, workspace-write, or full-access"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    FullAccess,
    MacOsSeatbelt,
    LinuxLandlockSeccomp,
    ApprovalRequired,
}

pub fn select_backend(
    os: &str,
    seatbelt_available: bool,
    landlock_abi: Option<i32>,
    policy: SandboxPolicy,
) -> SandboxBackend {
    if policy == SandboxPolicy::FullAccess {
        return SandboxBackend::FullAccess;
    }
    match os {
        "macos" if seatbelt_available => SandboxBackend::MacOsSeatbelt,
        "linux" if landlock_abi.unwrap_or(0) >= 1 => SandboxBackend::LinuxLandlockSeccomp,
        _ => SandboxBackend::ApprovalRequired,
    }
}

pub fn native_backend(policy: SandboxPolicy) -> SandboxBackend {
    select_backend(
        std::env::consts::OS,
        *SEATBELT_AVAILABLE,
        landlock_abi(),
        policy,
    )
}

static SEATBELT_AVAILABLE: Lazy<bool> = Lazy::new(|| {
    #[cfg(target_os = "macos")]
    {
        StdCommand::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1) (allow default)", "/usr/bin/true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
});

pub fn build_seatbelt_profile(policy: SandboxPolicy, writable_roots: &[PathBuf]) -> Result<String> {
    // 行序即优先级：SBPL 中**后出现的匹配规则胜出**（见 com.apple.writeconfig.sb /
    // CryptegraftService.sb 等 Apple 官方 profile 的写法），所以必须
    // 先放全局 allow，再放具体 deny。若把 (allow file-read*) 放到 deny 之后，
    // 它会覆盖上面的敏感读 deny，那些规则将全部失效。
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (allow file-read*)\n\
         (deny network*)\n\
         (deny file-write*)\n\
         (allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\") (literal \"/dev/dtracehelper\"))\n",
    );
    profile.push_str(&seatbelt_sensitive_read_denies()?);
    if policy == SandboxPolicy::WorkspaceWrite {
        let mut temp_roots = vec![PathBuf::from("/private/tmp"), PathBuf::from("/tmp")];
        let env_temp = std::env::temp_dir();
        if let Ok(canon_temp) = env_temp.canonicalize() {
            if !temp_roots.contains(&canon_temp) {
                temp_roots.push(canon_temp);
            }
        }
        if !temp_roots.contains(&env_temp) {
            temp_roots.push(env_temp);
        }
        for temp_root in &temp_roots {
            let escaped = escape_seatbelt_path(temp_root)?;
            profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
        }

        for root in writable_roots {
            append_seatbelt_writable_root(&mut profile, root)?;
            if let Ok(canonical) = root.canonicalize() {
                if canonical != *root {
                    append_seatbelt_writable_root(&mut profile, &canonical)?;
                }
            }
        }
    }
    Ok(profile)
}

/// 受限策略（ReadOnly / WorkspaceWrite）下禁止读取的凭据与敏感系统文件。
/// 对应 `PathPolicy::builtin_sensitive_paths` 中的凭据目录，以及 shadow/sudoers 等
/// 系统密钥文件——全局 `(allow file-read*)` 只放行其余非敏感路径。
fn seatbelt_sensitive_read_denies() -> Result<String> {
    let mut rules = String::new();
    let mut subpaths: Vec<PathBuf> = Vec::new();
    let mut literals: Vec<PathBuf> = vec![
        PathBuf::from("/etc/shadow"),
        PathBuf::from("/etc/sudoers"),
        PathBuf::from("/etc/master.passwd"),
    ];
    if let Some(home) = dirs::home_dir() {
        for dir in [
            ".ssh",
            ".aws",
            ".gnupg",
            ".kube",
            ".azure",
            ".config/gcloud",
        ] {
            subpaths.push(home.join(dir));
        }
        literals.push(home.join(".netrc"));
    }
    for path in &subpaths {
        rules.push_str(&format!(
            "(deny file-read* (subpath \"{}\"))\n",
            escape_seatbelt_path(path)?
        ));
    }
    for path in &literals {
        rules.push_str(&format!(
            "(deny file-read* (literal \"{}\"))\n",
            escape_seatbelt_path(path)?
        ));
    }
    Ok(rules)
}

fn append_seatbelt_writable_root(profile: &mut String, root: &Path) -> Result<()> {
    let escaped = escape_seatbelt_path(root)?;
    if root.is_file() {
        profile.push_str(&format!("(allow file-write* (literal \"{escaped}\"))\n"));
    } else {
        profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
    }
    Ok(())
}

fn escape_seatbelt_path(path: &Path) -> Result<String> {
    let raw = path.to_str().ok_or_else(|| {
        Error::Validation("Seatbelt path must be valid UTF-8 without raw bytes".to_string())
    })?;
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\n' | '\r' | '\0' => {
                return Err(Error::Validation(
                    "Seatbelt path contains forbidden control character".to_string(),
                ));
            }
            c if c.is_control() => {
                return Err(Error::Validation(
                    "Seatbelt path contains forbidden control character".to_string(),
                ));
            }
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            c => escaped.push(c),
        }
    }
    Ok(escaped)
}

impl SandboxBackend {
    /// 该后端是否真正执行了给定的策略。
    pub fn enforces(self, policy: SandboxPolicy) -> bool {
        match self {
            Self::FullAccess => policy == SandboxPolicy::FullAccess,
            Self::MacOsSeatbelt | Self::LinuxLandlockSeccomp => true,
            Self::ApprovalRequired => false,
        }
    }
}

/// 一次沙箱化进程启动请求。`policy` 由上层安全 contract 决定，provider 只负责执行。
#[derive(Debug, Clone)]
pub struct SandboxRequest {
    pub agent_id: String,
    pub session_id: String,
    pub policy: SandboxPolicy,
    /// `WorkspaceWrite` 时允许写入的根目录。
    pub writable_roots: Vec<PathBuf>,
    pub program: String,
    pub args: Vec<String>,
}

/// provider 准备好的、尚未启动的进程。
pub struct SandboxHandle {
    pub command: Command,
    pub backend: SandboxBackend,
}

/// 沙箱实现（本机 Seatbelt / Landlock，未来可以是 Docker / VM / Remote）。
///
/// provider 不能放宽请求中的策略：无法执行时必须返回错误，而不是降级为不受限执行。
#[async_trait::async_trait]
pub trait SandboxProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn prepare(&self, request: SandboxRequest) -> Result<SandboxHandle>;
}

/// 通过 provider 准备进程，并校验 provider 返回的后端确实执行了请求的策略。
pub async fn prepare_sandboxed(
    provider: &dyn SandboxProvider,
    request: SandboxRequest,
) -> Result<SandboxHandle> {
    let policy = request.policy;
    let handle = provider.prepare(request).await?;
    if !handle.backend.enforces(policy) {
        return Err(Error::PermissionDenied(format!(
            "sandbox provider '{}' returned backend {:?} that does not enforce policy {}",
            provider.name(),
            handle.backend,
            policy.as_str()
        )));
    }
    Ok(handle)
}

/// 本机原生沙箱：macOS Seatbelt / Linux Landlock + seccomp；不可用时要求审批。
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeSandboxProvider;

#[async_trait::async_trait]
impl SandboxProvider for NativeSandboxProvider {
    fn name(&self) -> &'static str {
        "native"
    }

    async fn prepare(&self, request: SandboxRequest) -> Result<SandboxHandle> {
        let SandboxRequest {
            policy,
            writable_roots,
            program,
            args,
            ..
        } = request;
        let backend = native_backend(policy);
        let mut command = match backend {
            SandboxBackend::FullAccess => {
                let mut command = Command::new(&program);
                command.args(&args);
                command
            }
            SandboxBackend::MacOsSeatbelt => {
                let profile = build_seatbelt_profile(policy, &writable_roots)?;
                let mut command = Command::new("/usr/bin/sandbox-exec");
                command.arg("-p").arg(profile).arg(&program).args(&args);
                command
            }
            SandboxBackend::LinuxLandlockSeccomp => {
                let roots = if policy == SandboxPolicy::ReadOnly {
                    Vec::new()
                } else {
                    writable_roots
                };
                let mut command = Command::new(&program);
                command.args(&args);
                // SAFETY: the closure performs only async-signal-safe syscalls in the
                // child immediately before exec and captures owned path buffers.
                unsafe {
                    command.pre_exec(move || apply_linux_sandbox(&roots));
                }
                command
            }
            SandboxBackend::ApprovalRequired => {
                return Err(Error::PermissionDenied(APPROVAL_REQUIRED.to_string()));
            }
        };
        command.kill_on_drop(true);
        Ok(SandboxHandle { command, backend })
    }
}

#[cfg(target_os = "linux")]
fn landlock_abi() -> Option<i32> {
    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    (result >= 1).then_some(result as i32)
}

#[cfg(not(target_os = "linux"))]
fn landlock_abi() -> Option<i32> {
    None
}

#[cfg(target_os = "linux")]
fn apply_linux_sandbox(writable_roots: &[PathBuf]) -> std::io::Result<()> {
    apply_landlock(writable_roots)?;
    apply_network_seccomp()
}

#[cfg(not(target_os = "linux"))]
fn apply_linux_sandbox(_writable_roots: &[PathBuf]) -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_landlock(writable_roots: &[PathBuf]) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::RawFd;

    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: i32 = 1;
    const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
    const ACCESS_FS_REFER: u64 = 1 << 13;
    const ACCESS_FS_TRUNCATE: u64 = 1 << 14;
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    if abi < 1 {
        return Err(std::io::Error::last_os_error());
    }
    let mut handled = ACCESS_FS_WRITE_FILE
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_MAKE_CHAR
        | ACCESS_FS_MAKE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_SOCK
        | ACCESS_FS_MAKE_FIFO
        | ACCESS_FS_MAKE_BLOCK
        | ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        handled |= ACCESS_FS_REFER;
    }
    if abi >= 3 {
        handled |= ACCESS_FS_TRUNCATE;
    }

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }
    #[repr(C)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
        reserved: u32,
    }

    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        ) as RawFd
    };
    if ruleset_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let result = (|| {
        for root in writable_roots {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            let path = CString::new(canonical.as_os_str().as_encoded_bytes())
                .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
            let path_fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if path_fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let allowed = if canonical.is_file() {
                handled & (ACCESS_FS_WRITE_FILE | ACCESS_FS_TRUNCATE)
            } else {
                handled
            };
            let rule = PathBeneathAttr {
                allowed_access: allowed,
                parent_fd: path_fd,
                reserved: 0,
            };
            let add_result = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    ruleset_fd,
                    RULE_PATH_BENEATH,
                    &rule,
                    0u32,
                )
            };
            unsafe { libc::close(path_fd) };
            if add_result < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u32) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })();
    unsafe { libc::close(ruleset_fd) };
    result
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeccompInstruction {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct SeccompArchSpec {
    pub audit_arch: u32,
    pub is_big_endian: bool,
    pub reject_x32: bool,
    /// 参数 0 为地址族的 socket 创建调用（socket / socketpair）：仅放行 AF_UNIX / AF_NETLINK。
    /// 其余 socket 操作（connect / bind / sendmsg / ...）有意不拦截——seccomp-BPF 无法
    /// 解引用 fd 检查其 domain，而沙箱内能自行创建的 socket 已被这里限定为本地 IPC。
    /// 残留风险仅限外部进程通过 fd 继承 / SCM_RIGHTS 向沙箱内注入网络 socket。
    pub domain_filtered_syscalls: Vec<u32>,
    pub af_unix: u32,
    pub af_netlink: u32,
    pub deny_errno: u32,
    /// 无条件拒绝的调用：主要是 32 位 x86 的 SYS_socketcall 复用入口
    /// （其实参指针无法在 BPF 中解引用，只能整体封禁——旧式二进制的本地 IPC 也随之禁用）。
    pub blocked_syscalls: Vec<u32>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LINUX_EPERM: u32 = 1;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LINUX_AF_UNIX: u32 = 1;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LINUX_AF_NETLINK: u32 = 16;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn seccomp_arch_spec(
    arch: &str,
    is_big_endian: bool,
) -> std::io::Result<SeccompArchSpec> {
    let deny_errno = SECCOMP_RET_ERRNO | LINUX_EPERM;
    match arch {
        "x86_64" if !is_big_endian => Ok(SeccompArchSpec {
            audit_arch: 0xc000_003e,
            is_big_endian: false,
            reject_x32: true,
            // socket(41), socketpair(53)
            domain_filtered_syscalls: vec![41, 53],
            af_unix: LINUX_AF_UNIX,
            af_netlink: LINUX_AF_NETLINK,
            deny_errno,
            blocked_syscalls: Vec::new(),
        }),
        "aarch64" if !is_big_endian => Ok(SeccompArchSpec {
            audit_arch: 0xc000_00b7,
            is_big_endian: false,
            reject_x32: false,
            // socket(198), socketpair(199)
            domain_filtered_syscalls: vec![198, 199],
            af_unix: LINUX_AF_UNIX,
            af_netlink: LINUX_AF_NETLINK,
            deny_errno,
            blocked_syscalls: Vec::new(),
        }),
        "x86" if !is_big_endian => Ok(SeccompArchSpec {
            audit_arch: 0x4000_0003,
            is_big_endian: false,
            reject_x32: false,
            // socket(359), socketpair(360) — direct syscalls added in Linux 4.3
            domain_filtered_syscalls: vec![359, 360],
            af_unix: LINUX_AF_UNIX,
            af_netlink: LINUX_AF_NETLINK,
            deny_errno,
            // Legacy multiplexed SYS_socketcall(102): its argument pointer cannot be
            // dereferenced in BPF, so it is blocked outright — otherwise it would be a
            // trivial bypass for the domain filter (and for all other socket ops).
            blocked_syscalls: vec![102],
        }),
        "arm" if !is_big_endian => Ok(SeccompArchSpec {
            audit_arch: 0x4000_0028,
            is_big_endian: false,
            reject_x32: false,
            // socket(281), socketpair(286)
            domain_filtered_syscalls: vec![281, 286],
            af_unix: LINUX_AF_UNIX,
            af_netlink: LINUX_AF_NETLINK,
            deny_errno,
            blocked_syscalls: Vec::new(),
        }),
        "riscv64" if !is_big_endian => Ok(SeccompArchSpec {
            audit_arch: 0xc000_00f3,
            is_big_endian: false,
            reject_x32: false,
            // socket(198), socketpair(199) — asm-generic numbering
            domain_filtered_syscalls: vec![198, 199],
            af_unix: LINUX_AF_UNIX,
            af_netlink: LINUX_AF_NETLINK,
            deny_errno,
            blocked_syscalls: Vec::new(),
        }),
        other => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("seccomp network filter is not supported on architecture '{other}'"),
        )),
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn build_network_seccomp_filter(spec: &SeccompArchSpec) -> Vec<SeccompInstruction> {
    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_JGE: u16 = 0x30;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;
    const SECCOMP_DATA_NR_OFFSET: u32 = 0;
    const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;

    let arg0_low_offset: u32 = if spec.is_big_endian { 20 } else { 16 };
    let mut filter = Vec::<SeccompInstruction>::with_capacity(
        spec.domain_filtered_syscalls.len() * 6 + spec.blocked_syscalls.len() * 2 + 16,
    );

    // 1. Always verify architecture to prevent multi-arch ABI bypass (e.g. int 0x80 on x86_64)
    filter.push(SeccompInstruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: SECCOMP_DATA_ARCH_OFFSET,
    });
    filter.push(SeccompInstruction {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 1,
        jf: 0,
        k: spec.audit_arch,
    });
    filter.push(SeccompInstruction {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: spec.deny_errno,
    });

    // 2. Load syscall number
    filter.push(SeccompInstruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: SECCOMP_DATA_NR_OFFSET,
    });

    // Reject x32 ABI syscalls on x86_64 (__X32_SYSCALL_BIT = 0x40000000)
    if spec.reject_x32 {
        filter.push(SeccompInstruction {
            code: BPF_JMP | BPF_JGE | BPF_K,
            jt: 0,
            jf: 1,
            k: 0x4000_0000,
        });
        filter.push(SeccompInstruction {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: spec.deny_errno,
        });
    }

    // 3. For each domain-filtered socket creation syscall (socket / socketpair), allow
    //    local IPC domains (AF_UNIX, AF_NETLINK) and block network domains (AF_INET, ...).
    //    其余 socket 操作不在此拦截——见 SeccompArchSpec::domain_filtered_syscalls 的说明。
    for &syscall in &spec.domain_filtered_syscalls {
        filter.push(SeccompInstruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 5,
            k: syscall,
        });
        filter.push(SeccompInstruction {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: arg0_low_offset,
        });
        filter.push(SeccompInstruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 1,
            jf: 0,
            k: spec.af_unix,
        });
        filter.push(SeccompInstruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: spec.af_netlink,
        });
        filter.push(SeccompInstruction {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });
        filter.push(SeccompInstruction {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: spec.deny_errno,
        });
    }

    // 4. Block unconditionally denied syscalls (e.g. legacy SYS_socketcall on x86)
    for &syscall in &spec.blocked_syscalls {
        filter.push(SeccompInstruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: syscall,
        });
        filter.push(SeccompInstruction {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: spec.deny_errno,
        });
    }
    filter.push(SeccompInstruction {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    filter
}

#[cfg(target_os = "linux")]
fn apply_network_seccomp() -> std::io::Result<()> {
    const SECCOMP_SET_MODE_FILTER: u32 = 1;

    let spec = seccomp_arch_spec(std::env::consts::ARCH, cfg!(target_endian = "big"))?;
    let instructions = build_network_seccomp_filter(&spec);
    let mut filter: Vec<libc::sock_filter> = instructions
        .into_iter()
        .map(|ins| libc::sock_filter {
            code: ins.code,
            jt: ins.jt,
            jf: ins.jf,
            k: ins.k,
        })
        .collect();
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::syscall(libc::SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0u32, &program) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_backend_selection_degrades_to_approval_when_unavailable() {
        assert_eq!(
            select_backend("macos", true, None, SandboxPolicy::WorkspaceWrite),
            SandboxBackend::MacOsSeatbelt
        );
        assert_eq!(
            select_backend("linux", false, Some(3), SandboxPolicy::WorkspaceWrite),
            SandboxBackend::LinuxLandlockSeccomp
        );
        assert_eq!(
            select_backend("windows", false, None, SandboxPolicy::WorkspaceWrite),
            SandboxBackend::ApprovalRequired
        );
        assert_eq!(
            select_backend("macos", false, None, SandboxPolicy::WorkspaceWrite),
            SandboxBackend::ApprovalRequired
        );
        assert_eq!(
            select_backend("linux", false, None, SandboxPolicy::FullAccess),
            SandboxBackend::FullAccess
        );
    }

    #[test]
    fn seatbelt_profile_denies_network_and_limits_writes_to_scope() {
        let workspace = PathBuf::from("/tmp/blockcell workspace");
        let scope = vec![workspace.join("src"), workspace.join("Cargo.toml")];

        let profile =
            build_seatbelt_profile(SandboxPolicy::WorkspaceWrite, &scope).expect("build profile");

        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("/tmp/blockcell workspace/src"));
        assert!(profile.contains("/tmp/blockcell workspace/Cargo.toml"));
        assert!(!profile.contains("(allow file-write*)"));
    }

    /// 违反 contract 的 provider：无论请求什么策略都返回不受限进程。
    struct PermissiveProvider;

    #[async_trait::async_trait]
    impl SandboxProvider for PermissiveProvider {
        fn name(&self) -> &'static str {
            "permissive"
        }
        async fn prepare(&self, request: SandboxRequest) -> Result<SandboxHandle> {
            Ok(SandboxHandle {
                command: Command::new(request.program),
                backend: SandboxBackend::FullAccess,
            })
        }
    }

    fn request(policy: SandboxPolicy) -> SandboxRequest {
        SandboxRequest {
            agent_id: "default".into(),
            session_id: "cli:test".into(),
            policy,
            writable_roots: vec![PathBuf::from("/tmp")],
            program: "sh".into(),
            args: vec!["-c".into(), "true".into()],
        }
    }

    #[tokio::test]
    async fn provider_cannot_weaken_requested_policy() {
        let error = prepare_sandboxed(&PermissiveProvider, request(SandboxPolicy::WorkspaceWrite))
            .await
            .err()
            .expect("weakened policy must be rejected");
        assert!(error.to_string().contains("does not enforce"), "{error}");

        let handle = prepare_sandboxed(&PermissiveProvider, request(SandboxPolicy::FullAccess))
            .await
            .expect("full-access request may run unrestricted");
        assert_eq!(handle.backend, SandboxBackend::FullAccess);
    }

    #[test]
    fn read_only_seatbelt_profile_has_no_write_allowance() {
        let profile =
            build_seatbelt_profile(SandboxPolicy::ReadOnly, &[PathBuf::from("/tmp/workspace")])
                .expect("build profile");

        assert!(profile.contains("(deny file-write*)"));
        assert!(!profile.contains("file-write* (subpath"));
        assert!(profile.contains("(literal \"/dev/null\")"));
        assert!(!profile.contains("/tmp/workspace"));
    }

    #[test]
    fn seatbelt_profile_denies_sensitive_reads() {
        let profile = build_seatbelt_profile(SandboxPolicy::ReadOnly, &[]).expect("build profile");
        // 全局 file-read* 保留（命令需要加载二进制/库），但凭据类路径显式 deny
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("(deny file-read*"));
        assert!(profile.contains("(literal \"/etc/shadow\")"));
        if let Some(home) = dirs::home_dir() {
            let ssh = escape_seatbelt_path(&home.join(".ssh")).unwrap();
            assert!(
                profile.contains(&format!("(subpath \"{ssh}\")")),
                "profile should deny reads under ~/.ssh: {profile}"
            );
        }

        // 顺序决定成败：SBPL 后出现的匹配规则胜出，所以 (allow file-read*) 必须排在
        // 所有 (deny file-read* ...) 之前，否则这些 deny 会被覆盖、完全失效。
        let allow_at = profile
            .find("(allow file-read*)")
            .expect("profile must allow file reads");
        let first_deny_at = profile
            .find("(deny file-read*")
            .expect("profile must deny sensitive reads");
        assert!(
            allow_at < first_deny_at,
            "(allow file-read*) must precede sensitive read denies, otherwise the denies are \
             overridden: {profile}"
        );
    }

    #[test]
    fn seatbelt_rejects_control_characters_instead_of_rewriting_paths() {
        let bad_newline = PathBuf::from("/tmp/bad\n)\n(allow file-write*)\n\"path");
        assert!(escape_seatbelt_path(&bad_newline).is_err());
        assert!(build_seatbelt_profile(SandboxPolicy::WorkspaceWrite, &[bad_newline]).is_err());

        let bad_nul = PathBuf::from("/tmp/bad\0path");
        assert!(escape_seatbelt_path(&bad_nul).is_err());

        let valid_quotes = PathBuf::from("/tmp/with\"quote\\slash");
        let escaped = escape_seatbelt_path(&valid_quotes).expect("valid path");
        assert_eq!(escaped, "/tmp/with\\\"quote\\\\slash");
    }

    fn simulate_seccomp(
        filter: &[SeccompInstruction],
        arch: u32,
        nr: u32,
        arg0: u32,
        is_big_endian: bool,
    ) -> u32 {
        let arg0_offset = if is_big_endian { 20 } else { 16 };
        let mut acc = 0u32;
        let mut pc = 0usize;
        while pc < filter.len() {
            let ins = filter[pc];
            match ins.code {
                // BPF_LD | BPF_W | BPF_ABS
                0x20 => {
                    acc = match ins.k {
                        0 => nr,
                        4 => arch,
                        off if off == arg0_offset => arg0,
                        _ => 0,
                    };
                    pc += 1;
                }
                // BPF_JMP | BPF_JEQ | BPF_K
                0x15 => {
                    let step = if acc == ins.k { ins.jt } else { ins.jf } as usize;
                    pc += 1 + step;
                }
                // BPF_JMP | BPF_JGE | BPF_K
                0x35 => {
                    let step = if acc >= ins.k { ins.jt } else { ins.jf } as usize;
                    pc += 1 + step;
                }
                // BPF_RET | BPF_K
                0x06 => return ins.k,
                other => panic!("unexpected BPF opcode: {other:#x}"),
            }
        }
        panic!("BPF program did not terminate with RET");
    }

    #[test]
    fn seccomp_filter_fails_closed_on_unknown_arch_and_blocks_x32_and_socketcall() {
        assert!(seccomp_arch_spec("sparc64", true).is_err());
        assert!(seccomp_arch_spec("x86_64", true).is_err());

        let x64 = seccomp_arch_spec("x86_64", false).expect("x86_64 spec");
        let x64_filter = build_network_seccomp_filter(&x64);
        let deny = SECCOMP_RET_ERRNO | LINUX_EPERM;

        // Foreign ABI (e.g. i386 int 0x80 on x86_64) is rejected
        assert_eq!(
            simulate_seccomp(&x64_filter, 0x4000_0003, 0, 0, false),
            deny
        );
        // x32 ABI syscall bit is rejected
        assert_eq!(
            simulate_seccomp(&x64_filter, x64.audit_arch, 0x4000_0000 | 41, 1, false),
            deny
        );
        // socket(41): local IPC domains (AF_UNIX=1, AF_NETLINK=16) allowed, AF_INET=2 denied
        assert_eq!(
            simulate_seccomp(&x64_filter, x64.audit_arch, 41, 1, false),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            simulate_seccomp(&x64_filter, x64.audit_arch, 41, 16, false),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            simulate_seccomp(&x64_filter, x64.audit_arch, 41, 2, false),
            deny
        );
        // socketpair(53) is domain-filtered the same way
        assert_eq!(
            simulate_seccomp(&x64_filter, x64.audit_arch, 53, 1, false),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            simulate_seccomp(&x64_filter, x64.audit_arch, 53, 2, false),
            deny
        );
        // Socket ops on already-created (local-only) fds are allowed: connect(42),
        // sendmsg(46), recvmsg(47), sendmmsg(307). Local IPC stays functional.
        for nr in [42u32, 46, 47, 307] {
            assert_eq!(
                simulate_seccomp(&x64_filter, x64.audit_arch, nr, 0, false),
                SECCOMP_RET_ALLOW,
                "syscall {nr} should stay allowed for local IPC"
            );
        }
        assert_eq!(
            simulate_seccomp(&x64_filter, x64.audit_arch, 0, 0, false),
            SECCOMP_RET_ALLOW
        );

        // 32-bit x86 blocks multiplexed SYS_socketcall (102)
        let x86 = seccomp_arch_spec("x86", false).expect("x86 spec");
        let x86_filter = build_network_seccomp_filter(&x86);
        assert_eq!(
            simulate_seccomp(&x86_filter, x86.audit_arch, 102, 1, false),
            deny
        );
    }
}
