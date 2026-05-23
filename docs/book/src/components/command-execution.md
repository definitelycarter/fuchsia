# Command Execution Design

This document covers the design for allowing WASM components to execute host binaries securely.

## Overview

WASI does not support spawning subprocesses. To allow components to use tools like `ffmpeg`, `imagemagick`, etc., we provide a custom host import (`platform` interface) that executes binaries on the host with OS-level sandboxing.

## Trust Model

The security model has multiple layers:

1. **WASM sandbox** - The component itself runs in wasmtime's sandbox
2. **Capability declaration** - Component manifest declares which commands it needs
3. **Workflow author approval** - User reviews and approves the declared capabilities
4. **Host validation** - Host checks requested command against declared `allowed_commands`
5. **OS sandbox** - Spawned binary runs inside `sandbox-exec` (macOS) or `bwrap` (Linux)

## WIT Interface

```wit
interface platform {
    record process-output {
        exit-code: s32,
        stdout: list<u8>,
        stderr: list<u8>,
    }

    /// Returns the sandboxed working directory for this execution
    work-dir: func() -> string;

    /// Execute a declared command
    execute: func(program: string, args: list<string>) -> result<process-output, string>;
}
```

## Manifest Capabilities

```yaml
capabilities:
  allowed_hosts: ["api.example.com"]
  allowed_paths: ["/tmp/workspace"]
  allowed_commands: ["ffmpeg", "imagemagick"]
```

## Work Directory

Each node execution gets an isolated work directory:

```
/tmp/fuchsia/executions/{execution_id}/{node_id}/
```

This directory is:
- The **only writable path** for spawned commands
- The **only readable path** (besides system libraries) for spawned commands
- Where the engine **stages inputs** before execution
- Where components **write outputs** for downstream nodes

Components retrieve this path via `platform::work_dir()`.

## Solving Write Access

**Problem**: A malicious component could specify arbitrary output paths:
```
ffmpeg -i input.mp4 -o /etc/cron.d/malicious
```

**Solution**: The sandbox profile only allows writes to the work directory. Any write outside fails at the OS level.

```scheme
(allow file-write* (subpath "/tmp/fuchsia/executions/{exec_id}/{node_id}"))
```

Components are expected to use `work_dir()` for outputs. If they don't, the write fails.

## Solving Read Access

**Problem**: A spawned binary could read sensitive host files:
```
ffmpeg -i ~/.ssh/id_rsa -f mp3 out.mp3
```

**Solution**: The sandbox profile only allows reads from:
1. System libraries (required for binaries to run)
2. The work directory (where inputs are staged)

```scheme
; System libs
(allow file-read* (subpath "/usr/lib"))
(allow file-read* (subpath "/System/Library"))
; ... etc

; Work directory only
(allow file-read* (subpath "/tmp/fuchsia/executions/{exec_id}/{node_id}"))
```

Inputs must be explicitly staged into the work directory by the engine. The component cannot access arbitrary host paths.

## macOS Sandbox Profile

Full working profile for macOS `sandbox-exec`:

```scheme
(version 1)
(deny default)

; System libs needed for binary to run
(allow file-read* (subpath "/usr/lib"))
(allow file-read* (subpath "/System/Library"))
(allow file-read* (subpath "/System/Cryptexes"))
(allow file-read* (subpath "/Library/Frameworks"))
(allow file-read* (subpath "/opt/homebrew/Cellar"))

; Allow reading metadata from anywhere (just attributes, not contents)
(allow file-read-metadata)
(allow file-read-data (literal "/"))

; Allow reading the binary (resolved via: which ffmpeg | xargs realpath)
(allow file-read* (literal "/opt/homebrew/Cellar/ffmpeg/8.0.1_1/bin/ffmpeg"))

; Work directory - read and write
(allow file-read* (subpath "/tmp/fuchsia/executions/{exec_id}/{node_id}"))
(allow file-write* (subpath "/tmp/fuchsia/executions/{exec_id}/{node_id}"))

; Only allow executing the declared binary
(allow process-exec (literal "/opt/homebrew/Cellar/ffmpeg/8.0.1_1/bin/ffmpeg"))
(allow process-fork)

; System access needed for process to function
(allow sysctl-read)
(allow mach-lookup)
```

### Key Implementation Notes

1. **Resolve symlinks**: `sandbox-exec` requires real paths. Use `std::fs::canonicalize()` to resolve symlinks:
   ```rust
   let which_path = which::which("ffmpeg")?;           // /opt/homebrew/bin/ffmpeg (symlink)
   let real_path = std::fs::canonicalize(which_path)?; // /opt/homebrew/Cellar/ffmpeg/8.0.1_1/bin/ffmpeg (real)

   // Use real_path in both file-read* and process-exec rules
   ```

2. **Metadata wildcard**: Use `(allow file-read-metadata)` without a path to allow reading file/directory attributes anywhere. This simplifies the profile by avoiding the need to enumerate every directory level for path traversal.

3. **Root directory**: The root `/` needs `file-read-data` (not just metadata) to list its contents for path resolution.

4. **`mach-lookup`**: Required for IPC with macOS system services. Without it, most binaries abort.

5. **Homebrew paths**: Binaries installed via Homebrew need read access to `/opt/homebrew/Cellar` where the actual binaries and dynamic libraries live.

### File Read Permission Types

| Permission | What it allows |
|------------|----------------|
| `file-read-metadata` | Read attributes (size, permissions, timestamps) |
| `file-read-data` | Read file contents or directory listings |
| `file-read*` | Wildcard - all read operations |

## Linux Sandbox (Future)

Linux would use `bubblewrap` (bwrap) with similar restrictions:

```bash
bwrap \
  --ro-bind /usr /usr \
  --ro-bind /lib /lib \
  --ro-bind /lib64 /lib64 \
  --bind /tmp/fuchsia/executions/{exec_id}/{node_id} /work \
  --unshare-net \
  --die-with-parent \
  /usr/bin/ffmpeg -i /work/input.mp4 -o /work/output.mp4
```

Combined with `seccomp` filters for syscall restrictions.

## Alternative: WASM-Native Tools

Instead of spawning native binaries, tools compiled to WASM could run inside the existing sandbox:

- [Wasmer ffmpeg](https://wasmer.io/wasmer/ffmpeg) - ffmpeg compiled to WASM

**Caveat**: Many WASM-compiled tools use WASIX (Wasmer's extended WASI), which isn't compatible with wasmtime. As the WASI ecosystem matures, this may become viable.

## Alternative: Structured APIs

Instead of raw command execution, provide typed APIs:

```wit
interface media {
    transcode: func(input: blob, format: output-format) -> result<blob, string>;
    extract-audio: func(input: blob) -> result<blob, string>;
}
```

This eliminates path manipulation entirely - data goes in and out as blobs, host manages all file I/O internally.

Tradeoff: More work to design each operation, less flexibility.

## Open Questions

1. **Argument validation**: Should we validate args in Rust before execution, or rely purely on sandbox enforcement?

2. **Network access for commands**: Should spawned binaries have network access? Current profile denies it.

3. **Resource limits**: Timeout, max output size, CPU/memory limits on spawned processes?

4. **Stderr handling**: Should stderr from commands be logged, returned to component, or discarded?

## References

- [Anthropic sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime) - Cross-platform sandboxing in Node.js
- [Apple Sandbox Guide](https://developer.apple.com/library/archive/documentation/Darwin/Reference/ManPages/man7/sandbox.7.html) - macOS sandbox-exec documentation
- [Bubblewrap](https://github.com/containers/bubblewrap) - Linux unprivileged sandboxing
