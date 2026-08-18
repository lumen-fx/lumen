# bash completion for lumenc.
#
# Load it into the current shell:
#
#     source lumenc.bash
#
# or install it where bash-completion looks for it, for example
# ~/.local/share/bash-completion/completions/lumenc.
#
# `lumenc completions bash` prints this file.

# Every subcommand lumenc dispatches. Kept in step with `lumenc --help` by
# crates/lumenc/tests/completions.rs.
_lumenc_commands="run check build new fmt snapshot find element-at click type key scroll lint diff screenshot web bundle package i18n completions"

_lumenc() {
    local cur prev cmd i word
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD - 1]}"

    # The subcommand is the first word that is not a flag.
    cmd=""
    for ((i = 1; i < COMP_CWORD; i++)); do
        word="${COMP_WORDS[i]}"
        case "$word" in
            -*) ;;
            *)
                cmd="$word"
                break
                ;;
        esac
    done

    if [ -z "$cmd" ]; then
        mapfile -t COMPREPLY < <(compgen -W "$_lumenc_commands --help --version" -- "$cur")
        return
    fi

    # Flags that take a value, keyed by subcommand: `--text` names a mode for
    # `snapshot` and takes a string for `find`.
    case "$cmd $prev" in
        "run --profile")
            mapfile -t COMPREPLY < <(compgen -W "chrome tracy stderr" -- "$cur")
            return
            ;;
        "click --button")
            mapfile -t COMPREPLY < <(compgen -W "primary secondary middle" -- "$cur")
            return
            ;;
        "web --render")
            mapfile -t COMPREPLY < <(compgen -W "static csr" -- "$cur")
            return
            ;;
        "web --prerender")
            mapfile -t COMPREPLY < <(compgen -W "seeds run none" -- "$cur")
            return
            ;;
        "package --target")
            mapfile -t COMPREPLY < <(compgen -W "linux-x86_64 linux-aarch64 macos-x86_64 macos-aarch64 windows-x86_64" -- "$cur")
            return
            ;;
        "run --artifact" | "run --assets" | "screenshot --bounds")
            mapfile -t COMPREPLY < <(compgen -f -- "$cur")
            return
            ;;
        *" --app" | "package --lib-dir" | "web --out" | "web --lib-dir")
            mapfile -t COMPREPLY < <(compgen -d -- "$cur")
            return
            ;;
        "run --size" | "run --dpr" | "run --ticks" | "i18n --lang" | "package --name" | \
        "web --base" | "web --locale" | "web --port" | "web --host" | "web --allow-host" | \
        "find --text" | "find --role" | "find --id" | "find --limit" | \
        "snapshot --max-lines" | "snapshot --cursor" | "screenshot --highlight" | \
        *" --port" | *" --wait-for")
            return
            ;;
    esac

    local flags=""
    case "$cmd" in
        run) flags="--profile --headless --size --dpr --ticks --artifact --assets --no-hooks" ;;
        check) flags="" ;;
        build) flags="--no-hooks" ;;
        new) flags="--list -l" ;;
        fmt) flags="--check" ;;
        snapshot) flags="--text --json --max-lines --cursor --include-invisible --no-omit-invisible --port --app" ;;
        find) flags="--text --role --id --limit --json --port --app" ;;
        element-at) flags="--json --port --app" ;;
        click) flags="--button --wait-for --json --port --app" ;;
        type) flags="--wait-for --json --port --app" ;;
        key) flags="--shift --ctrl --alt --super --cmd --wait-for --json --port --app" ;;
        scroll) flags="--wait-for --json --port --app" ;;
        lint) flags="--css-cascade --signals --strict --json --port --app" ;;
        diff) flags="--json --port --app" ;;
        screenshot) flags="--highlight --lint --bounds --port --app" ;;
        web) flags="--out --base --locale --render --prerender --no-hooks --lib-dir --strict --serve --ssr --port --host --allow-host" ;;
        bundle) flags="--static --no-hooks" ;;
        package) flags="--name --target --lib-dir --no-hooks" ;;
        i18n) flags="--lang" ;;
        completions) flags="" ;;
    esac

    if [[ "$cur" == -* ]]; then
        mapfile -t COMPREPLY < <(compgen -W "$flags --help" -- "$cur")
        return
    fi

    # Positional arguments: how many have been typed already decides what the
    # next one is.
    local -a positionals=()
    for ((i = 1; i < COMP_CWORD; i++)); do
        word="${COMP_WORDS[i]}"
        case "$word" in
            -*) ;;
            *) positionals+=("$word") ;;
        esac
    done
    local count=$((${#positionals[@]} - 1))

    case "$cmd" in
        run | check | lint | web)
            mapfile -t COMPREPLY < <(compgen -d -- "$cur")
            ;;
        build | bundle | package)
            # <app_dir> first, then an output path.
            if [ "$count" -eq 0 ]; then
                mapfile -t COMPREPLY < <(compgen -d -- "$cur")
            else
                mapfile -t COMPREPLY < <(compgen -f -- "$cur")
            fi
            ;;
        new)
            # <name> is new, so only the template argument completes.
            if [ "$count" -eq 1 ]; then
                mapfile -t COMPREPLY < <(compgen -W "blank hello counter form todo dashboard settings hotkeys" -- "$cur")
            fi
            ;;
        fmt)
            mapfile -t COMPREPLY < <(compgen -f -X '!*.lmn' -- "$cur")
            ;;
        i18n)
            if [ "$count" -eq 0 ]; then
                mapfile -t COMPREPLY < <(compgen -W "extract" -- "$cur")
            else
                mapfile -t COMPREPLY < <(compgen -d -- "$cur")
            fi
            ;;
        completions)
            if [ "$count" -eq 0 ]; then
                mapfile -t COMPREPLY < <(compgen -W "bash zsh fish" -- "$cur")
            fi
            ;;
        screenshot)
            mapfile -t COMPREPLY < <(compgen -f -- "$cur")
            ;;
    esac
}

complete -F _lumenc lumenc
