FROM alpine:3.20

# openssh-server-pam adds /usr/sbin/sshd.pam (+ /etc/pam.d/sshd), used by
# the kbdint role; the plain sshd binary serves every other role.
RUN apk add --no-cache openssh openssh-server-pam iproute2 \
    && adduser -D test \
    # A password must exist for pubkey login to work at all on Alpine
    # (locked accounts refuse every auth method). Servers that should not
    # accept it set PasswordAuthentication no in their role config.
    && echo 'test:pw-secret-1' | chpasswd

COPY configs/ /etc/ssh/roles/
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]
