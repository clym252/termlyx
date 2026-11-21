# The pinned OpenSSH test server plus tmux, for the reattach tests, and fish,
# a login shell whose quoting differs from POSIX sh's (the tests run the
# startup commands through it with docker exec).
FROM lscr.io/linuxserver/openssh-server:10.3_p1-r1-ls239
RUN apk add --no-cache tmux fish
# Copied in rather than bind-mounted so the image also works on a remote
# Docker host (scripts/devvm.sh), which cannot see this checkout. The image
# runs the executable scripts in /custom-cont-init.d after its own init and
# before sshd starts; ours enables TCP forwarding. They must be root-owned
# (COPY's default). The private key stays out of the context (.dockerignore).
COPY keys/id_ed25519_test.pub /keys/id_ed25519_test.pub
COPY sshd-init/ /custom-cont-init.d/
