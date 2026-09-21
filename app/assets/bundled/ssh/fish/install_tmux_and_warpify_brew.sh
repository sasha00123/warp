brew install tmux

if test $status -eq 0
    tmux -Lwarp -CC new-session -A -s (_warp_tmux_session_name)
    exit
end
