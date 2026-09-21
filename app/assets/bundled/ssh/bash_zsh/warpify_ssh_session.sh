_find() {
    command -v "$1" >/dev/null 2>&1
}

_log() {
    _msg=$(printf "{\"hook\": \"$1\", \"value\": $2}" | command -p od -An -v -tx1 | command -p tr -d " \n")
    printf '\033\120\044\144%s\234' "$_msg"
}

_err() {
    _log RemoteWarpificationIsUnavailable "$1"
}

_system_details() {
    OS=$(uname)
    if [ "$OS" = "Darwin" ]; then
        if _find brew; then
            PKG="homebrew"
        fi
    elif [ "$OS" = "Linux" ]; then
        if _find pacman; then
            PKG="pacman"
        elif _find zypper; then
            PKG="zypper"
        elif _find dnf; then
            PKG="dnf"
        elif _find yum && _find yumdownloader; then
            PKG="yum"
        elif _find apt; then
            PKG="apt"
        fi
    fi
    RA="no_root_access"
    if command -v sudo >/dev/null && { sudo -vn && sudo -ln; } 2>&1 | grep -E 'may run|a password' > /dev/null; then RA="can_run_sudo"
    elif [ "$(id -u)" -eq 0 ] && [ "$(whoami)" = "root" ]; then RA="is_root"
    fi

    WH=$( [ -w ~ ] && echo true || echo false )

    printf '%s' "{\"os\": \"$OS\", \"pkg\": \"$PKG\", \"shell\": \"$(basename $SHELL)\", \"root_access\": \"$RA\", \"writable_home\": $WH}"
}

  # _check_tmux is used in tmux install script post install!
_check_tmux() {
    if _find $HOME/.warp/tmux/execute_tmux.sh; then
        _log SshTmuxInstaller "\"warp\""
        TMUX="$HOME/.warp/tmux/execute_tmux.sh"
    elif _find tmux; then
        TMUX="tmux"
        _log SshTmuxInstaller "\"user\""
    fi

    if [ $TMUX ]; then
        VER=$(command $TMUX -V 2>/dev/null | awk '{print $2}')
        if [ -z "$VER" ]; then
            _err "\"TmuxFailed\""
        elif [ "$(printf '%s\n' "$VER" "2.9" | sort -V | tail -n1)" = "2.9" ]; then
            _err "{\"UnsupportedTmuxVersion\": $(_system_details)}"
        else
            return 0
        fi
    else
        _err "{\"TmuxNotInstalled\": $(_system_details)}"
    fi
    return 1
}

_warp_tmux_session_name() {
    if [ -n "$WARP_SSH_TMUX_SESSION" ]; then
        _session_name="$WARP_SSH_TMUX_SESSION"
    else
        _host=$(command -p hostname 2>/dev/null || uname -n)
        _user=$(command -p whoami 2>/dev/null || printf '%s' "$USER")
        _session_name="warp-${_user}-${_host}"
    fi

    _sanitized_session_name=$(printf '%s' "$_session_name" | command -p tr -c 'A-Za-z0-9_.-' '-' | command -p cut -c1-80)
    if [ -n "$_sanitized_session_name" ]; then
        printf '%s' "$_sanitized_session_name"
    else
        printf '%s' "warp-default"
    fi
}

_warp_tmux_window_name() {
    _ts=$(date +%s 2>/dev/null || printf '%s' "$$")
    printf 'warp-%s-%s' "$_ts" "$$"
}

_warp_tmux_state_dir() {
    _dir="$HOME/.warp/tmux"
    mkdir -p "$_dir" 2>/dev/null || true
    printf '%s' "$_dir"
}

_warp_tmux_state_file() {
    _session_name="$1"
    _safe_name=$(printf '%s' "$_session_name" | command -p tr -c 'A-Za-z0-9_.-' '-' | command -p cut -c1-80)
    printf '%s/%s.target' "$(_warp_tmux_state_dir)" "$_safe_name"
}

_warp_tmux_target_exists() {
    _session_name="$1"
    _window_id="$2"
    _pane_id="$3"
    [ -n "$_window_id" ] && [ -n "$_pane_id" ] && command $TMUX -Lwarp list-panes -s -t "$_session_name" -F '#{window_id} #{pane_id}' 2>/dev/null | command -p grep -Fx "$_window_id $_pane_id" >/dev/null 2>&1
}

_warp_tmux_save_target() {
    _session_name="$1"
    _window_id="$2"
    _pane_id="$3"
    printf '%s %s\n' "$_window_id" "$_pane_id" > "$(_warp_tmux_state_file "$_session_name")" 2>/dev/null || true
}

_warp_tmux_create_target() {
    _session_name="$1"
    _window_name="$(_warp_tmux_window_name)"

    if command $TMUX -Lwarp has-session -t "$_session_name" 2>/dev/null; then
        command $TMUX -Lwarp set-environment -u -t "$_session_name" WARP_BOOTSTRAPPED 2>/dev/null || true
        command $TMUX -Lwarp set-environment -u -t "$_session_name" WARP_BOOTSTRAP_VAR 2>/dev/null || true
        command $TMUX -Lwarp set-option -g history-limit 1000000 2>/dev/null || true
        command $TMUX -Lwarp set-window-option -g remain-on-exit off 2>/dev/null || true
        command $TMUX -Lwarp new-window -d -P -F '#{window_id} #{pane_id}' -t "$_session_name:" -n "$_window_name"
    else
        command $TMUX -Lwarp new-session -d -s "$_session_name" -n warp-bootstrap || return 1
        command $TMUX -Lwarp set-option -g history-limit 1000000 2>/dev/null || true
        command $TMUX -Lwarp set-window-option -g remain-on-exit off 2>/dev/null || true
        _target=$(command $TMUX -Lwarp new-window -d -P -F '#{window_id} #{pane_id}' -t "$_session_name:" -n "$_window_name") || return 1
        command $TMUX -Lwarp kill-window -t "$_session_name:warp-bootstrap" 2>/dev/null || true
        printf '%s\n' "$_target"
    fi
}

_warp_tmux_first_target() {
    _session_name="$1"
    command $TMUX -Lwarp list-panes -s -t "$_session_name" -F '#{window_id} #{pane_id}' 2>/dev/null | command -p sed -n '1p'
}

_warp_tmux_print_picker() {
    _session_name="$1"
    _target_file="$2"
    _last_window_id="$3"
    _last_pane_id="$4"

    command $TMUX -Lwarp list-panes -s -t "$_session_name" -F '#{pane_id}|#{window_id}|#{window_name}|#{pane_current_command}|#{pane_current_path}|#{pane_active}' > "$_target_file" 2>/dev/null || true

    printf '\nWarp tmux workspaces for %s\n' "$_session_name" >&2
    printf '  n) New window\n' >&2
    _idx=1
    while IFS='|' read -r _pane_id _window_id _window_name _command _path _active; do
        [ -z "$_pane_id" ] && continue
        _marker=' '
        [ "$_window_id" = "$_last_window_id" ] && [ "$_pane_id" = "$_last_pane_id" ] && _marker='*'
        [ "$_active" = "1" ] && _active_label="active" || _active_label="pane"
        printf ' %s %s) %s %s  %s  %s\n' "$_marker" "$_idx" "$_window_name" "$_pane_id" "$_command" "$_path" >&2
        _idx=$((_idx + 1))
    done < "$_target_file"
    printf 'Choose workspace [Enter: new, r: resume starred, number]: ' >&2
}

_warp_tmux_choose_target() {
    _session_name="$1"
    _state_file="$(_warp_tmux_state_file "$_session_name")"
    _last_window_id=""
    _last_pane_id=""
    if [ -r "$_state_file" ]; then
        _last_window_id=$(command -p sed -n '1p' "$_state_file" | command -p awk '{print $1}')
        _last_pane_id=$(command -p sed -n '1p' "$_state_file" | command -p awk '{print $2}')
    fi

    if [ "$WARP_SSH_TMUX_MODE" = "new" ] || [ "$WARP_SSH_TMUX_NEW" = "1" ]; then
        _warp_tmux_create_target "$_session_name"
        return
    fi

    if [ -n "$WARP_SSH_TMUX_WINDOW_ID" ] && [ -n "$WARP_SSH_TMUX_PANE_ID" ] && _warp_tmux_target_exists "$_session_name" "$WARP_SSH_TMUX_WINDOW_ID" "$WARP_SSH_TMUX_PANE_ID"; then
        printf '%s %s\n' "$WARP_SSH_TMUX_WINDOW_ID" "$WARP_SSH_TMUX_PANE_ID"
        return
    fi

    if ! command $TMUX -Lwarp has-session -t "$_session_name" 2>/dev/null; then
        _warp_tmux_create_target "$_session_name"
        return
    fi

    if [ "$WARP_SSH_TMUX_MODE" = "resume" ] && _warp_tmux_target_exists "$_session_name" "$_last_window_id" "$_last_pane_id"; then
        printf '%s %s\n' "$_last_window_id" "$_last_pane_id"
        return
    fi

    if [ "$WARP_SSH_TMUX_MODE" != "pick" ]; then
        _warp_tmux_create_target "$_session_name"
        return
    fi

    _picker_file="$(_warp_tmux_state_dir)/picker.$$"
    _warp_tmux_print_picker "$_session_name" "$_picker_file" "$_last_window_id" "$_last_pane_id"

    _choice=""
    if [ -n "$BASH_VERSION" ] || [ -n "$ZSH_VERSION" ]; then
        read -r -t "${WARP_SSH_TMUX_PICK_TIMEOUT:-8}" _choice 2>/dev/null || _choice=""
    else
        read -r _choice 2>/dev/null || _choice=""
    fi
    printf '\n' >&2

    if [ "$_choice" = "n" ] || [ "$_choice" = "N" ] || [ "$_choice" = "new" ] || [ "$_choice" = "NEW" ] || [ -z "$_choice" ]; then
        rm -f "$_picker_file" 2>/dev/null || true
        _warp_tmux_create_target "$_session_name"
        return
    elif [ "$_choice" = "r" ] || [ "$_choice" = "R" ] || [ "$_choice" = "resume" ] || [ "$_choice" = "RESUME" ]; then
        if _warp_tmux_target_exists "$_session_name" "$_last_window_id" "$_last_pane_id"; then
            rm -f "$_picker_file" 2>/dev/null || true
            printf '%s %s\n' "$_last_window_id" "$_last_pane_id"
            return
        fi
        rm -f "$_picker_file" 2>/dev/null || true
        _warp_tmux_create_target "$_session_name"
        return
    else
        _selected=$(command -p sed -n "${_choice}p" "$_picker_file" 2>/dev/null)
        _pane_id=$(printf '%s' "$_selected" | command -p cut -d'|' -f1)
        _window_id=$(printf '%s' "$_selected" | command -p cut -d'|' -f2)
        rm -f "$_picker_file" 2>/dev/null || true
        if _warp_tmux_target_exists "$_session_name" "$_window_id" "$_pane_id"; then
            printf '%s %s\n' "$_window_id" "$_pane_id"
            return
        fi
        _warp_tmux_create_target "$_session_name"
        return
    fi
}

_warp_tmux_attach() {
    _session_name="$(_warp_tmux_session_name)"
    unset WARP_BOOTSTRAPPED

    _target="$(_warp_tmux_choose_target "$_session_name")"
    _window_id=$(printf '%s' "$_target" | command -p awk '{print $1}')
    _pane_id=$(printf '%s' "$_target" | command -p awk '{print $2}')
    _warp_tmux_target_exists "$_session_name" "$_window_id" "$_pane_id" || return 1

    command $TMUX -Lwarp set-option -t "$_session_name" destroy-unattached off 2>/dev/null || true
    command $TMUX -Lwarp set-option -t "$_session_name" allow-passthrough on 2>/dev/null || true
    command $TMUX -Lwarp set-option -g history-limit 1000000 2>/dev/null || true
    command $TMUX -Lwarp set-window-option -g remain-on-exit off 2>/dev/null || true
    command $TMUX -Lwarp set-environment -u -t "$_session_name" WARP_BOOTSTRAPPED 2>/dev/null || true
    command $TMUX -Lwarp select-window -t "$_window_id" 2>/dev/null || true
    command $TMUX -Lwarp select-pane -t "$_pane_id" 2>/dev/null || true
    _warp_tmux_save_target "$_session_name" "$_window_id" "$_pane_id"
    command $TMUX -Lwarp -CC attach-session -t "$_session_name"
}

_check_tmux && _warp_tmux_attach && exit
