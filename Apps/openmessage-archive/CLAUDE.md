# CLAUDE.md: OpenMessage Archive

The guide for this directory is [`AGENTS.md`](AGENTS.md), imported below. Edit
that file; keep this one to Claude Code specifics.

@AGENTS.md

## Claude Code specifics

### Smoke-test the real app

Unit tests do not cover the CSP, the capability grants, or IPC wiring. After
touching any of those, run the release binary under Xvfb and look at it:

```sh
npx tauri build --no-bundle
S="$(mktemp -d)"
XDG_CONFIG_HOME="$S/config" WEBKIT_DISABLE_DMABUF_RENDERER=1 \
WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
xvfb-run -a -s "-screen 0 1360x860x24" bash -c '
  ./target/release/openmessage-archive & sleep 12
  import -window root "'"$S"'/shot.png"; kill %1'
```

Then read `$S/shot.png`.

Three tools help drive the app:

- `xdotool mousemove X Y click 1` gives the window focus.
- `xdotool key ctrl+comma` sends shortcuts once the window has focus.
- `import` comes from ImageMagick.

Two failure screens are easy to misread:

- **A blank light-grey window** means WebKit has not painted yet. The first
  launch after a boot is slow, so wait longer.
- **"Could not connect to localhost"** means the binary was built without the
  Tauri CLI.

### Reviews and research

- For an architecture review before a milestone lands, use the read-only
  coroner subagent: `/coroner:coroner-review Apps/openmessage-archive standard`.
- For format research on backups, iOS versions or tools, cite primary
  sources. Add findings to `docs/supported-formats.md` with **UNVERIFIED**
  markers where needed, rather than relying on memory.
