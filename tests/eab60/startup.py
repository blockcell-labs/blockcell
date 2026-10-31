"""Isolated gateway survival probe; uses an existing binary and never builds it."""
from eab60 import Gateway, WORKSPACE
from run_all import apply_profile


def main():
    # Keep default disabled channel settings to exercise listener fallback.
    WORKSPACE.mkdir(parents=True, exist_ok=True)
    apply_profile("base")
    gateway = Gateway("startup.log")
    try:
        gateway.start()
        gateway.assert_survives(seconds=5)
    finally:
        gateway.stop()


if __name__ == "__main__":
    main()
