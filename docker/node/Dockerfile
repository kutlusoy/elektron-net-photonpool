# Elektron Net node built from source. ELEKTRON_REF is the branch "photonpool" (it contains the getattestationbatch RPC) until that code is on main;
# then switch it to main. Use a tag or commit for reproducible builds.
FROM debian:bookworm-slim AS build
ARG ELEKTRON_REPO=https://github.com/kutlusoy/elektron-net.git
ARG ELEKTRON_REF=photonpool
# Photon node patches (getattestationbatch, no consensus code path is changed). Set to false to build the unmodified node;
# the pool then falls back to one getblocktemplate call per payout address.
ARG PHOTON_NODE_PATCHES=false
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake pkgconf python3 git \
        libevent-dev libboost-dev libsqlite3-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
RUN git clone --depth 1 --branch "${ELEKTRON_REF}" "${ELEKTRON_REPO}" src
COPY patches /patches
RUN if [ "${PHOTON_NODE_PATCHES}" = "true" ]; then \
        for p in /patches/*.patch; do \
            git -C src apply "$p" || { echo "ERROR: $p does not apply to ${ELEKTRON_REF}; build with PHOTON_NODE_PATCHES=false to skip it"; exit 1; }; \
        done; \
    fi
RUN cmake -S src -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_GUI=OFF -DBUILD_TESTS=OFF \
        -DENABLE_WALLET=ON -DENABLE_IPC=OFF -DWITH_ZMQ=OFF \
    && cmake --build build -j"$(nproc)" --target elektrond elektron-cli \
    && install -m 0755 build/bin/elektrond build/bin/elektron-cli /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        libevent-2.1-7 libevent-pthreads-2.1-7 libevent-extra-2.1-7 libsqlite3-0 libstdc++6 ca-certificates gosu \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r -g 10001 elektron && useradd -r -u 10001 -g elektron -d /data -s /usr/sbin/nologin elektron \
    && mkdir -p /data && chown elektron:elektron /data
COPY --from=build /usr/local/bin/elektrond /usr/local/bin/elektron-cli /usr/local/bin/
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
# Strip carriage returns in case the repository was checked out with Windows line endings (see .gitattributes): a "#!/bin/sh\r" shebang
# makes the container fail with "exec /usr/local/bin/entrypoint.sh: no such file or directory".
RUN sed -i 's/\r$//' /usr/local/bin/entrypoint.sh && chmod +x /usr/local/bin/entrypoint.sh
VOLUME ["/data"]
EXPOSE 8333
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["elektrond", "-conf=/data/elektron.conf", "-datadir=/data", "-printtoconsole"]
