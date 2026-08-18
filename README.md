# llmq

Ask an LLM from your fish prompt and drop the answer straight into the command
line. Alacritty + fish + starship. A single static-ish Rust binary — no runtime
to install, and it paints in about 5 ms.

```
┌─ llmq ─────────────────────────────────────────┐
│ ask> find every python file changed today      │
│ ───────────────────────────────────────────────│
│ find . -name '*.py' -mtime -1                  │  ← selected
│ fd -e py --changed-within 1d                   │
│ ───────────────────────────────────────────────│
│ enter insert · ^a insert all · ^x insert+run   │
└────────────────────────────────────────────────┘
```

## Install

Build it, then add one line to your fish config:

```fish
cd ~/src/llmq && cargo build --release

echo 'source ~/src/llmq/llmq.fish' >> ~/.config/fish/config.fish
```

`llmq.fish` locates the binary relative to itself via `status
--current-filename`, so there is no symlink into PATH and no `conf.d` entry to
manage. It prefers `./llmq` (copy the binary there if you want to throw the
build tree away) and falls back to `./target/release/llmq`, so a working tree
needs no install step. Move or rename the folder and it keeps working, as long
as `llmq.fish` and the binary stay together.

Building needs a Rust toolchain. Running it needs nothing at all — no
interpreter, no shared libraries beyond libc. `tmux` is optional; without it
the TUI simply draws inline.

Then the config:

```fish
mkdir -p ~/.config/llmq
cp ~/src/llmq/config.example.toml ~/.config/llmq/config.toml
$EDITOR ~/.config/llmq/config.toml   # set url + key
```

Reload with `exec fish`. `alt+/` works immediately. For a nicer chord, add the keybinding to `~/.config/alacritty/alacritty.toml`:

```toml
[[keyboard.bindings]]
key = "Slash"
mods = "Control|Shift"
chars = "\u001b[99~"
```

`ctrl+shift+/` is the chord because a bare `?` is already `shift+/` and can't be
bound without breaking typing. `alt+/` is bound as a fallback that needs no
Alacritty config. To use a different chord, run `fish_key_reader`, press it, and
bind whatever it prints in `llmq.fish`.

If the script lives somewhere unusual, set `LLMQ_BIN` *before* the source line.
`set -g LLMQ_NOBIND 1` skips the default bindings so you can define your own.

## Keys

| key | |
|---|---|
| `enter` | send the question, then insert the highlighted line |
| `↑` `↓` / `k` `j` | move between answer lines |
| `^a` | insert the whole answer |
| `^x` | insert and run immediately |
| `e` `/` | go back and edit the question |
| `esc` | cancel |

## How it works

1. **Alacritty** turns the chord into an escape sequence (`\e[99~`) — it has no
   overlay API of its own, so the key has to arrive as bytes in the tty stream.
2. **fish** binds that sequence to `_llmq_widget`, which calls `llmq` inside a
   command substitution and passes the current buffer as context.
3. **llmq** saves `fd 1`, then points `fd 1` at `/dev/tty` so crossterm paints
   the TUI on the terminal while the capture pipe stays clean. The chosen text
   is written back to the saved descriptor at exit. (Atuin gets the same effect
   with `3>&1 1>&2 2>&3`; this is the same trick with fewer moving parts.)
4. **fish** puts the result in the buffer with `commandline -r --`, and
   `commandline -f execute` if the answer carries the `__llmq_accept__:` prefix.

Inside tmux it shells out to `tmux display-popup -E` and passes the result back
through a temp file, so you get a real floating window. Set `[tmux] enabled =
false` (or pass `--no-popup`) to always draw inline.

## Notes

- The system prompt in `config.toml` is what makes answers paste-able. Loosen it
  if you want prose instead of bare commands.
- `key_command = "pass show openai"` keeps the API key out of the config file.
- `llmq --print-config` shows the effective config after merging defaults.
- Streaming is plain SSE parsing over `ureq`, so any OpenAI-compatible server
  works — including a local llama.cpp or Ollama with no key at all.
- `cargo test` covers the config merge, key resolution and text wrapping.
- `[api.extra]` is merged straight into the request body, for provider knobs
  llmq has no field of its own for.

If you point llmq at a reasoning model it will look like it has hung: the
answer only appears once the model has finished thinking, and llmq throws the
thinking away. Turn it off — on OpenRouter that took `qwen3.7-flash` from a
5301 ms median to 686 ms:

```toml
[api.extra]
reasoning = { enabled = false }
```

The spelling is provider-specific: OpenAI's own endpoint wants
`reasoning_effort = "minimal"`, and `reasoning = { exclude = true }` merely
hides the reasoning — you still wait for it.

## Speed

Startup, measured on this machine — keypress to a painted, ready-to-type box:

| | inline | inside tmux |
|---|---|---|
| Rust | 5 ms | 49 ms |
| Python (v0.1) | 149 ms | 405 ms |

The tmux column is larger because the popup runs a second copy of the binary
inside `display-popup`; most of that 49 ms is tmux's own popup machinery.

## Ideas next

- Feed `atuin search --limit 20 --cmd-only` in as extra context.
- A second binding that explains the command already on the line instead of
  writing a new one (same widget, different system prompt).
- Cache the last answer so a repeat press reopens the browser without re-asking.

## Licence

Apache 2.0 — see [LICENSE](LICENSE).
