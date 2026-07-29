# A containerised gns3server that mirrors the VM the experiments were originally run on:
# Ubuntu 20.04 / Python 3.9 / gns3-server 2.2.55 / ubridge 0.9.19.
#
# The container does NOT run its own Docker daemon. GNS3 router nodes are created as *sibling*
# containers on the host's daemon through the mounted /var/run/docker.sock. That is why the
# compose file runs this with `pid: host` and `privileged: true`: ubridge has to enter the network
# namespace of those sibling containers (via /proc/<pid>/ns/net) to wire up the veth pairs that
# form the emulated topology.
FROM ubuntu:20.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        python3.9 python3.9-venv python3.9-dev \
        gcc libc6-dev make git \
        libpcap-dev \
        busybox-static \
        iproute2 iputils-ping net-tools procps \
        curl ca-certificates gnupg \
    && rm -rf /var/lib/apt/lists/*

# ubridge, pinned to the version present on the reference VM.
RUN git clone --depth 1 --branch v0.9.19 https://github.com/GNS3/ubridge.git /tmp/ubridge \
    && make -C /tmp/ubridge \
    && make -C /tmp/ubridge install \
    && rm -rf /tmp/ubridge

# Docker CLI only: it talks to the host daemon over the bind-mounted socket.
RUN install -m0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc \
    && chmod a+r /etc/apt/keyrings/docker.asc \
    && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu focal stable" \
        > /etc/apt/sources.list.d/docker.list \
    && apt-get update && apt-get install -y --no-install-recommends docker-ce-cli \
    && rm -rf /var/lib/apt/lists/*

RUN python3.9 -m venv /opt/gns3-venv \
    && /opt/gns3-venv/bin/pip install --no-cache-dir --upgrade pip setuptools wheel \
    && /opt/gns3-venv/bin/pip install --no-cache-dir gns3-server==2.2.55

COPY gns3_server.conf /root/.config/GNS3/2.2/gns3_server.conf

EXPOSE 3080

CMD ["/opt/gns3-venv/bin/gns3server"]
