# Mechanics sheet

Status: the Craft Anything decompiler tools were not reachable in this run, so nothing below was read from
D:\UltraVNC\vncviewer.exe's code. Every value is the public RFB (VNC) protocol standard or a simplification chosen
for the "very, very easy" request. All are marked estimated.

## Protocol (RFB 3.3 / 3.7 / 3.8) - estimated
- Default port 5900; a number under 100 after the colon means display number (5900 + n) - estimated
- Version reply: highest of 3.8 / 3.7 / 3.3 the server offers - estimated
- Security: only 1 = none is used (password login was removed on request) - estimated
- Pixel format requested: 32 bits, colour depth 24, little endian, red shift 16, green shift 8, blue shift 0,
  max 255 each - estimated
- Pictures understood: Raw (0), CopyRect (1), DesktopSize (-223) - estimated (the original also offers Hextile,
  ZRLE, Tight etc.; left out to keep this simple)
- Messages sent: FramebufferUpdateRequest (3), KeyEvent (4), PointerEvent (5)
- Mouse button mask: left 1, middle 2, right 4, wheel up 8, wheel down 16 - estimated

## Simplifications (on purpose)
- One screen to connect (address + optional password + one Connect button), then the remote desktop.
- Two buttons only while connected: Disconnect, Send Ctrl+Alt+Del.
- No settings dialogs, no file transfer, no chat, no encryption plugins, no saved connections.

## Second app: Simple VNC Server (estimated, from the public RFB standard)
- Listens on port 5900; starts sharing by itself when opened; one button stops/starts it.
- No password: the server offers only open login (security type 1); the viewer also accepts only that. Anyone on the same network can connect - estimated/by request.
- Screen: whole main screen grabbed about every 30 ms, only the changed rows are sent (Raw picture type), mouse cursor drawn in.
- Pixel formats: any true-colour 8/16/32-bit format the viewer asks for.
- Mouse/keyboard: sent into Windows as normal input; wheel bits 8 (up) and 16 (down); X11 key codes mapped to Windows keys.
- Not covered: Ctrl+Alt+Del, locked screen, admin windows (needs a Windows service), clipboard, file transfer.

## Terminal options (by request)
- Viewer: `simple-vnc-viewer [address[:port]]`, `--help`, `--screenshot file.png`
- Server: `simple-vnc-server [--port N] [--minimized] [--help] [--screenshot file.png]`
