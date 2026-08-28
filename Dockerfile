# Two stages, and the second one is empty.
#
# Stela is a statically linked musl binary with no runtime dependencies, so the
# image it ships in needs to contain nothing else: no base distribution, no
# package manager, no shell. That is a security property before it is a size
# one — there is no shell to obtain, and no distribution CVEs to track, because
# there is no distribution.
#
# The cost, stated so it is a choice rather than a surprise: `docker exec` gives
# you nothing to run. Debug against the binary on a host, or swap this stage for
# `alpine` temporarily.

FROM clux/muslrust:1.95.0-stable AS build
WORKDIR /src
COPY . .
RUN CARGO_HOME=/src/.cargo cargo build --release --target x86_64-unknown-linux-musl \
    && strip target/x86_64-unknown-linux-musl/release/stela

FROM scratch
COPY --from=build /src/target/x86_64-unknown-linux-musl/release/stela /stela

# 0.0.0.0 rather than the 127.0.0.1 default: inside a container, binding
# loopback means nothing outside the container can reach it. The default stays
# loopback because that is the safe choice for a binary run on a host.
EXPOSE 3000
WORKDIR /blog
ENTRYPOINT ["/stela"]
CMD ["serve", "--host", "0.0.0.0"]
