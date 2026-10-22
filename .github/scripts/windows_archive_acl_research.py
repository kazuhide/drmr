"""Diagnostic-branch-only, bounded stderr classification for the fixed ACL probe."""

import os
import re
import subprocess
import threading
import time


LIMIT = 8192
EXCEPTIONS = (
    "MethodInvocationException", "UnauthorizedAccessException", "SecurityException",
    "IOException", "ArgumentException", "ArgumentNullException", "InvalidOperationException",
    "PlatformNotSupportedException", "NotSupportedException", "DirectoryNotFoundException",
    "FileNotFoundException", "Win32Exception", "ParameterBindingException", "ParserError",
    "CommandNotFoundException", "RuntimeException", "IdentityNotMappedException",
    "PrivilegeNotHeldException", "InvalidAclException",
)
OPERATIONS = ("SetAccessControl", "GetAccessControl", "GetAccessRules", "SetOwner", "SetAccessRuleProtection")
PHRASES = (
    ("access is denied", "AccessDenied"),
    ("attempted to perform an unauthorized operation", "AccessDenied"),
    ("some or all identity references could not be translated", "IdentityTranslationFailure"),
    ("a required privilege is not held by the client", "RequiredPrivilegeMissing"),
    ("this security id may not be assigned as the owner", "OwnerAssignmentRefused"),
    ("private acl verification failed", "PrivateAclVerificationFailed"),
    ("the handle is invalid", "InvalidHandle"),
    ("could not load file or assembly", "AssemblyLoadFailure"),
)
CATEGORIES = frozenset(EXCEPTIONS + OPERATIONS + tuple(value for _, value in PHRASES) +
                       ("NoStderr", "UnclassifiedStderr"))


def classify(data):
    """Return only fixed identifiers and explicitly labeled integer error codes."""
    text = data.decode("utf-8", errors="replace")
    categories = {value for value in EXCEPTIONS + OPERATIONS if value in text}
    categories.update(value for phrase, value in PHRASES if phrase in text.lower())
    if not categories:
        categories.add("UnclassifiedStderr" if data else "NoStderr")

    def codes(label, minimum, maximum):
        values = set()
        pattern = rf"\b(?:{label})\s*[:=]\s*(-?0x[0-9a-fA-F]{{1,8}}|-?[0-9]{{1,10}})\b"
        for value in re.findall(pattern, text, flags=re.IGNORECASE):
            parsed = int(value, 16 if "x" in value.lower() else 10)
            if minimum <= parsed <= maximum:
                values.add(parsed)
        return sorted(values)[:4]

    return {
        "categories": sorted(categories)[:16],
        "hresults": codes("HResult", -(2**31), 2**32 - 1),
        "win32_codes": codes("NativeErrorCode|Win32ErrorCode", 0, 2**32 - 1),
    }


def capture(original_run, publish, stage, command, *args, **kwargs):
    """Preserve run's result or exception; redirect only stderr to a bounded drain."""
    started = time.monotonic()
    retained = bytearray()
    counters = {"bytes": 0, "read_error": False}
    reader = writer = None
    thread = None
    status = "os-error"
    exit_code = None

    def drain():
        try:
            while True:
                data = os.read(reader, 4096)
                if not data:
                    break
                counters["bytes"] += len(data)
                retained.extend(data[:max(0, LIMIT - len(retained))])
        except OSError:
            counters["read_error"] = True
        finally:
            os.close(reader)

    try:
        try:
            reader, writer = os.pipe()
            thread = threading.Thread(target=drain, daemon=True)
            thread.start()
        except (OSError, RuntimeError):
            if reader is not None:
                os.close(reader)
            if writer is not None:
                os.close(writer)
            reader = writer = None
            thread = None
        observed = dict(kwargs)
        if writer is not None:
            observed["stderr"] = writer
        try:
            result = original_run(command, *args, **observed)
        except subprocess.TimeoutExpired:
            status = "timeout"
            raise
        except OSError:
            status = "os-error"
            raise
        else:
            exit_code = result.returncode
            status = "ok" if exit_code == 0 else "nonzero"
            return result
    finally:
        if writer is not None:
            os.close(writer)
        if thread is not None:
            thread.join(timeout=1)
        state = ("unavailable" if thread is None else "incomplete" if thread.is_alive()
                 else "read-error" if counters["read_error"] else "complete")
        row = dict(stage=stage, status=status, elapsed_ms=int((time.monotonic() - started) * 1000),
                   exit_code=exit_code, stderr_state=state, stderr_bytes=counters["bytes"],
                   stderr_retained_bytes=len(retained), stderr_truncated=counters["bytes"] > LIMIT,
                   **classify(bytes(retained)))
        try:
            publish(row)
        except (OSError, ValueError):
            pass
