"""Nonblocking process lock for a single mutable Genesis workspace."""
import fcntl
from functools import wraps
from pathlib import Path


class WorkspaceLock:
    def __init__(self, root):
        self.path = Path(root) / 'supervisor.lock'
        self.handle = None

    def __enter__(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.handle = self.path.open('a+')
        try:
            fcntl.flock(self.handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            self.handle.close()
            raise RuntimeError('Genesis supervisor already running for this workspace') from exc
        return self

    def __exit__(self, *args):
        self.handle.close()


def exclusive(method):
    @wraps(method)
    def locked(self, *args, **kwargs):
        if getattr(self, '_lock_depth', 0):
            return method(self, *args, **kwargs)
        with WorkspaceLock(self.root):
            self._lock_depth = 1
            try:
                self.reload_usage()
                return method(self, *args, **kwargs)
            finally:
                self._lock_depth = 0
    return locked
