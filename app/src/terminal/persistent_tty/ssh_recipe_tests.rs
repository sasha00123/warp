use super::*;

fn recipe(arguments: &[&str]) -> SshRecipe {
    SshRecipe { working_directory: "/Users/test/projects with spaces".into(),
        arguments: arguments.iter().map(|arg| (*arg).to_owned()).collect() }
}

#[test]
fn persistent_ssh_recipe_preserves_argument_boundaries_and_jump_hosts() {
    let original = recipe(&["-vv", "-p2222", "-F", "./ssh config", "-i", "keys/my key",
        "-J", "jump@bastion:2200", "user@host"]);
    let args = original.master_arguments(Path::new("/tmp/private/control")).unwrap();
    assert!(args.windows(2).any(|pair| pair == ["-F", "./ssh config"]));
    assert!(args.windows(2).any(|pair| pair == ["-i", "keys/my key"]));
    assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert!(args.windows(2).any(|pair| pair == ["-J", "jump@bastion:2200"]));
    assert_eq!(args.last().unwrap(), "user@host");
    assert_eq!(original.working_directory(), Path::new("/Users/test/projects with spaces"));
}

#[test]
fn persistent_ssh_recipe_does_not_clone_forwarding_or_user_control_sockets() {
    let args = recipe(&["-ttM", "-L8080:localhost:80", "-R", "9090:localhost:90",
        "-D1080", "-S", "/tmp/user-owned", "-oControlPath=/tmp/other", "-o", "ControlMaster=auto",
        "host"]).master_arguments(Path::new("/tmp/private/control")).unwrap();
    assert!(!args.iter().any(|arg| arg.contains("user-owned") || arg.contains("8080")
        || arg.contains("9090") || arg.contains("1080") || arg.contains("/tmp/other")));
    assert_eq!(args.iter().filter(|arg| *arg == "-S").count(), 1);
    assert!(!args.iter().any(|arg| arg == "-t" || arg == "-M"));
    assert!(args.iter().any(|arg| arg == "StrictHostKeyChecking=yes"));
}

#[test]
fn persistent_ssh_recipe_rejects_commands_control_operations_and_truncation() {
    for args in [vec!["host", "rm -rf ~"], vec!["-O", "exit", "host"], vec!["-G", "host"],
        vec!["-f", "host"], vec!["-W", "host:22", "jump"], vec!["-i"], vec!["--"], vec!["-p", "host"]] {
        assert!(recipe(&args).validate().is_err(), "accepted {args:?}");
    }
    assert!(SshRecipe::decode(b"EWSSH1\0/home/test\0host").is_err());
    assert!(SshRecipe::decode(b"EWSSH1\0relative\0host\0").is_err());
    assert!(SshRecipe::decode(&vec![0; 65537]).is_err());
}

#[test]
fn persistent_ssh_recipe_decodes_only_the_local_nul_delimited_format() {
    let decoded = SshRecipe::decode(b"EWSSH1\0/home/test\0-F\0config with spaces\0host\0").unwrap();
    assert_eq!(decoded.arguments, ["-F", "config with spaces", "host"]);
    assert!(SshRecipe::decode(b"{\"hook\":\"SSH\",\"args\":[\"host\"]}").is_err());
}

#[test]
fn persistent_recovery_is_guarded_and_does_not_clone_forwardings_or_start_a_job() {
    let id = "a".repeat(32);
    let generation = "b".repeat(32);
    let command = recipe(&["-F", "./ssh config", "-J", "jump@host", "-nNT", "-L9000:host:9",
        "-S", "/tmp/user-master", "-oRemoteCommand=bad", "remote"]).recovery_command(&id, &generation).unwrap();
    assert!(command.contains("/usr/bin/ssh '-tt'"));
    assert!(command.contains("./ssh config"));
    assert!(command.contains("jump@host"));
    assert!(command.contains("WARP_WORKSPACE_GENERATION"));
    assert!(command.contains(&generation));
    assert!(command.contains("attach-session"));
    assert!(command.contains(&format!("set-option -t ew-{id} status off")));
    assert!(command.contains(&format!("set-option -w -t =ew-{id}:0 window-size latest")));
    assert!(command.contains(&format!("attach-session -t ew-{id}")));
    assert!(!command.contains("new-session"));
    assert!(!command.contains("send-keys"));
    assert!(!command.contains("9000"));
    assert!(!command.contains("/tmp/user-master"));
    assert!(!command.contains("RemoteCommand=bad"));
    assert!(command.contains("StrictHostKeyChecking=yes"));
    for invalid in ["", "@1", "$(touch /tmp/unsafe)", &"a".repeat(33)] {
        assert!(recipe(&["host"]).recovery_command(invalid, &generation).is_err());
        assert!(recipe(&["host"]).recovery_command(&id, invalid).is_err());
    }
}
