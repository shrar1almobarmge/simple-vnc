# Simple VNC

A very simple VNC server and viewer for Windows, written in Rust.

- **simple-vnc-server** — shares your main screen on port 5900. Starts sharing as soon as it opens.
- **simple-vnc-viewer** — type an address, press Connect.

> No password: anyone on the same network can connect while the server is running.

## Install the server (PowerShell)

```powershell
irm https://github.com/shrar1almobarmge/simple-vnc/releases/latest/download/simple-vnc-server.exe -OutFile simple-vnc-server.exe; .\simple-vnc-server.exe
```

## Start with Windows, also before login

```powershell
.\simple-vnc-server.exe --install
```

Windows asks for permission (press Yes). The server is copied to `C:\Program Files\Simple VNC`, runs as a background service, opens its port in the firewall, and also shows the lock screen. To remove it: `.\simple-vnc-server.exe --uninstall`.

## Install the viewer (PowerShell)

```powershell
irm https://github.com/shrar1almobarmge/simple-vnc/releases/latest/download/simple-vnc-viewer.exe -OutFile simple-vnc-viewer.exe; .\simple-vnc-viewer.exe
```

## Options

- Server: `simple-vnc-server [--port N] [--help]` — runs in the terminal, prints `Connected: <ip>` when a viewer joins. Press Ctrl+C to stop.
- Viewer: `simple-vnc-viewer [address[:port]] [--help]`

## Build from source

```
cargo build --release
```
