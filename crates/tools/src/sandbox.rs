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

pub fn build_seatbelt_profile(policy: SandboxPolicy, writable_roots: &[PathBuf]) -> String {
    let mut profile = String::from(
        "(version 1)\n(allow default)\n(allow file-read*)\n(deny network*)\n(deny file-write*)\n",
    );
    if policy == SandboxPolicy::WorkspaceWrite {
        for root in writable_roots {
            let escaped = escape_seatbelt_path(root);
            if root.is_file() {
                profile.push_str(&format!("(allow file-write* (literal \"{escaped}\"))\n"));
            } else {
                profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
            }
        }
    }
    profile
}

fn escape_seatbelt_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
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
                let profile = build_seatbelt_profile(policy, &writable_roots);
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

#[cfg(target_os = "linux")]
fn apply_network_seccomp() -> std::io::Result<()> {
    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;
    const SECCOMP_SET_MODE_FILTER: u32 = 1;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

    let blocked = [
        libc::SYS_socket,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_sendto,
        libc::SYS_recvfrom,
        libc::SYS_sendmsg,
        libc::SYS_recvmsg,
    ];
    let mut filter = Vec::<libc::sock_filter>::with_capacity(blocked.len() * 2 + 2);
    filter.push(libc::sock_filter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 0,
    });
    for syscall in blocked {
        filter.push(libc::sock_filter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: syscall as u32,
        });
        filter.push(libc::sock_filter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
        });
    }
    filter.push(libc::sock_filter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
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

        let profile = build_seatbelt_profile(SandboxPolicy::WorkspaceWrite, &scope);

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
            build_seatbelt_profile(SandboxPolicy::ReadOnly, &[PathBuf::from("/tmp/workspace")]);

        assert!(profile.contains("(deny file-write*)"));
        assert!(!profile.contains("file-write* (subpath"));
        assert!(!profile.contains("file-write* (literal"));
    }
}
