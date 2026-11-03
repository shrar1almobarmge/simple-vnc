# Simple VNC

A very simple VNC server and viewer for Windows, written in Rust.

- **simple-vnc-server** — shares your main screen on port 5900. Starts sharing as soon as it opens.
- **simple-vnc-viewer** — type an address, press Connect.

> No password: anyone on the same network can connect while the server is running.

## Install the server (PowerShell)

```powershell
irm https://github.com/shrar1almobarmge/simple-vnc/releases/latest/download/simple-vnc-server.exe -OutFile simple-vnc-server.exe; .\simple-vnc-server.exe
```

## Install the viewer (PowerShell)

```powershell
irm https://github.com/shrar1almobarmge/simple-vnc/releases/latest/download/simple-vnc-viewer.exe -OutFile simple-vnc-viewer.exe; .\simple-vnc-viewer.exe
```

## Options

- Server: `simple-vnc-server [--port N] [--minimized] [--help]`
- Viewer: `simple-vnc-viewer [address[:port]] [--help]`

## Build from source

```
cargo build --release
```
