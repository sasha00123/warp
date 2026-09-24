FROM debian:bookworm-slim

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        openssh-server tmux bash ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash tester \
    && passwd -d tester \
    && mkdir -p /run/sshd /home/tester/.ssh \
    && chmod 700 /home/tester/.ssh \
    && chown -R tester:tester /home/tester/.ssh \
    && rm -f /etc/ssh/ssh_host_*

COPY sshd_config /etc/ssh/sshd_config

EXPOSE 22
CMD ["/bin/sh", "-ec", "ssh-keygen -A; install -m 600 -o tester -g tester /run/lab-authorized-key /home/tester/.ssh/authorized_keys; exec /usr/sbin/sshd -D -e"]

