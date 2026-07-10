FROM alpine:3.20

RUN apk add --no-cache openssh iproute2 \
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
