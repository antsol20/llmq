# llmq — fish integration
#
# Add to ~/.config/fish/config.fish:
#
#     source /path/to/llmq/llmq.fish
#
# Nothing else to install: this file finds the llmq script sitting next to it,
# so no symlink into PATH and no conf.d entry are required.
#
# Optional overrides, set BEFORE the source line:
#     set -g LLMQ_BIN   /custom/path/to/llmq   # use a different executable
#     set -g LLMQ_NOBIND 1                     # skip the default keybindings

# --- locate the executable -------------------------------------------------
# No `exit` / `return` at top level here: this file is meant to be sourced from
# config.fish, and bailing out early behaves differently across fish versions.

if not set -q LLMQ_BIN
    set -g LLMQ_BIN (realpath (dirname (status --current-filename)))/llmq
end

if not test -f "$LLMQ_BIN"
    # Fall back to PATH if the sibling script isn't there.
    if command -q llmq
        set -g LLMQ_BIN (command -v llmq)
    else
        set -g LLMQ_BIN ""
        echo "llmq: script not found next to "(status --current-filename)" and not on PATH" >&2
    end
end

# --- widget ----------------------------------------------------------------

function _llmq_widget --description "ask an LLM and drop the answer into the command line"
    test -z "$LLMQ_BIN"; and return

    # NB: `set -l` inside an if-block is scoped to that block, so declare first.
    set -l cmd $LLMQ_BIN
    test -x "$LLMQ_BIN"; or set cmd python3 $LLMQ_BIN

    # `string collect` keeps a multi-line answer as ONE element; otherwise fish
    # splits on newlines and `commandline -r` would join them with spaces.
    set -l out ($cmd --context (commandline -b) | string collect)

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
    set -l cmd $LLMQ_BIN
    test -x "$LLMQ_BIN"; or set cmd python3 $LLMQ_BIN
    $cmd $argv
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
