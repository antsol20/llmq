# AGENTS.md

Notes for an agent (or a human) picking this project up cold.

## What this is

`llmq` asks an OpenAI-compatible LLM from the fish prompt and drops the answer
straight into the command-line buffer. A single Rust binary plus one fish file.
Bound to `alt+/` (and `ctrl+shift+/` via an Alacritty escape sequence).

It was Python + curses until v0.2; the Rust rewrite is commit `1429f1a`. Nothing
Python remains — if you find a `.py` reference anywhere, it is stale.

## Layout

| file | what lives there |
|---|---|
| `src/main.rs` | arg parsing, tmux dispatch, the fd trick, panic hook, exit codes |
| `src/ui.rs` | crossterm TUI: ask box, streaming pane, line browser, key handling |
| `src/api.rs` | SSE streaming over `ureq`, sends `Msg`s down a channel |
| `src/config.rs` | TOML load + defaults, key resolution, endpoint URL normalisation |
| `llmq.fish` | the widget, binary discovery, keybindings |
| `config.example.toml` | template; the real one is `~/.config/llmq/config.toml` |

## Build and test

```
cargo build --release      # binary at target/release/llmq
cargo test                 # 9 unit tests: config merge, key resolution, wrapping
cargo clippy --all-targets
```

`llmq.fish` prefers `./llmq` next to itself, then `./target/release/llmq`, then
`$PATH`. A working tree needs no install step. Reload with `exec fish`.

## Invariants — break these and the tool silently misbehaves

**stdout is a pipe the shell pastes into.** The widget runs `llmq` inside a
command substitution, so *anything* printed on stdout lands in the user's
command line. Errors go to stderr. Diagnostics go to stderr. The only thing
that may reach stdout is the chosen answer.

**The fd trick.** `redirect_to_tty()` dups fd 1 aside, then points fd 1 at
`/dev/tty` so crossterm paints on the terminal while the capture pipe stays
clean. The result is written to the *saved* descriptor at exit. Don't "simplify"
this by writing to `println!`.

**Config fails loudly.** `config::load` returns `ConfigError::Missing` rather
than falling back to defaults. This is deliberate: the original bug report was
an HTTP 401 from `api.openai.com` caused by a missing config file silently
falling back to OpenAI defaults with an empty key. Never reintroduce a silent
default endpoint.

**Geometry clamps last.** In `ui.rs::geom()`, the minimum-size floor is applied
*before* the clamp to terminal size. Doing it the other way round yields a
negative origin on narrow terminals and crashes. Verified from 20x10 to 200x50.

**No clap.** Args are hand-parsed in `main.rs` because startup latency is the
whole point — the binary paints in ~5 ms inline, ~49 ms inside tmux.

**`ACCEPT_PREFIX` / `ERR_PREFIX`.** `__llmq_accept__:` on stdout means "insert
and run" (`^x`). `__llmq_error__:` is how the inner popup process reports a
failure back through the temp file, since the popup itself vanishes.

## Hard-won facts

- **tmux 3.6 closes popups on exit regardless of `-E` or `-EE`**, contradicting
  its own man page. Error text left on the popup's screen is unreadable — that
  is why failures round-trip through the temp file instead.
- **tmux is optional.** Without it the TUI draws inline over the pane.
- `capture-pane` reads the pane buffer, not the popup overlay, and `list-panes -a`
  does not enumerate popups. To test the popup path, attach a real client on a
  pty and use `display-popup`'s blocking behaviour as the detector.
- **`delta.reasoning` is discarded.** Models that stream hidden reasoning appear
  to hang with an empty box. See below.

## Model selection

Benchmarked live against OpenRouter with llmq's exact request shape, 12 calls
per model, interleaved so network drift hits all of them equally:

| model | TTFT med | total med | total p90 | total max | $/1k queries |
|---|---|---|---|---|---|
| **google/gemini-2.5-flash-lite** | 590 ms | **691 ms** | **806 ms** | **842 ms** | $0.0166 |
| qwen/qwen3-30b-a3b-instruct-2507 | 802 ms | 851 ms | 1264 ms | 6621 ms | $0.0069 |
| qwen/qwen3.7-flash *(reasoning off)* | 900 ms | 973 ms | 1384 ms | 1866 ms | $0.0063 |
| openai/gpt-4.1-nano | 780 ms | 976 ms | 1135 ms | 1367 ms | $0.0141 |

Gemini 2.5 Flash Lite is the default and wins on every latency metric; its tail
is what matters, since a line cannot be selected until the stream completes.

The dominant effect when evaluating *any* new model is reasoning tokens. llmq
discards `delta.reasoning`, so a thinking model looks like a hang. `qwen3.7-flash`
went from 6499 ms with half its answers empty to 973 ms purely by sending
`reasoning: {"enabled": false}`. Try that switch first. Note `gpt-5-nano` and
`gpt-oss-20b` reject it with HTTP 400 and want `effort` instead.

Caveat on quality: in the sample set Gemini returned `mv *.jpeg *.jpg` (broken
for more than one file) where all three alternatives returned a correct `for`
loop. One sample per query, so treat it as anecdote, not a benchmark.

## Known gaps

- **No `[api.extra]` passthrough.** There is no way to send arbitrary keys in the
  request body, which means `reasoning: {"enabled": false}` and
  `provider: {sort: "latency"}` cannot be set from config. Roughly 15 lines
  across `config.rs` and `api.rs`. Discussed, not approved, not built.
- The tmux dispatch spawns a second copy of the binary. Moving it into
  `llmq.fish` would remove one process from the popup path.

## Conventions

- Comments explain *why*, not *what*, and are sparse. Match the surrounding density.
- Commit messages: imperative mood, no `Co-Authored-By` trailer.
- Never commit a real API key. `config.example.toml` uses `sk-...`. One was
  leaked in early history — the key has been rotated.
