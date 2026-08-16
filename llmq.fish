# llmq — fish integration
#
# Add to ~/.config/fish/config.fish:
#
#     source /path/to/llmq/llmq.fish
#
# Nothing else to install: this file finds the llmq binary sitting next to it,
# so no symlink into PATH and no conf.d entry are required.
#
# Optional overrides, set BEFORE the source line:
#     set -g LLMQ_BIN   /custom/path/to/llmq   # use a different binary
#     set -g LLMQ_NOBIND 1                     # skip the default keybindings

# --- locate the executable -------------------------------------------------
# No `exit` / `return` at top level here: this file is meant to be sourced from
# config.fish, and bailing out early behaves differently across fish versions.

if not set -q LLMQ_BIN
    set -l _llmq_dir (realpath (dirname (status --current-filename)))
    # An installed binary sitting next to this file wins; otherwise use the
    # cargo build output, so a working tree needs no install step.
    for _llmq_try in $_llmq_dir/llmq $_llmq_dir/target/release/llmq
        if test -x "$_llmq_try"
            set -g LLMQ_BIN $_llmq_try
            break
        end
    end
end

if not set -q LLMQ_BIN; or not test -x "$LLMQ_BIN"
    if command -q llmq
        set -g LLMQ_BIN (command -v llmq)
    else
        set -g LLMQ_BIN ""
        echo "llmq: no binary next to "(status --current-filename)" and none on PATH." >&2
        echo "llmq: build one with 'cargo build --release' in that directory." >&2
    end
end

# --- widget ----------------------------------------------------------------

function _llmq_widget --description "ask an LLM and drop the answer into the command line"
    test -z "$LLMQ_BIN"; and return

    # `string collect` keeps a multi-line answer as ONE element; otherwise fish
    # splits on newlines and `commandline -r` would join them with spaces.
    set -l out ($LLMQ_BIN --context (commandline -b) | string collect)

    commandline -f repaint

    test -z "$out"; and return

    if string match -q '__llmq_accept__:*' -- $out
        # `--` guards against answers that begin with a dash
        commandline -r -- (string replace '__llmq_accept__:' '' -- $out)
        commandline -f execute
    else
        commandline -r -- $out
    end
end

function llmq --description "ask an LLM (wrapper around the llmq script)"
    test -z "$LLMQ_BIN"; and return 1
    $LLMQ_BIN $argv
end

# --- keybindings -----------------------------------------------------------

if not set -q LLMQ_NOBIND; and status is-interactive
    # \e[99~ is what the Alacritty binding in the README emits.
    bind \e\[99~ _llmq_widget
    bind \e/ _llmq_widget # alt+/ — works with no terminal config at all

    if bind -M insert >/dev/null 2>&1
        bind -M insert \e\[99~ _llmq_widget
        bind -M insert \e/ _llmq_widget
    end
end
