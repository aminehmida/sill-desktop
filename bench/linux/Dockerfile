# The Linux bench: builds the workspace and runs the tests and the app headless (Xvfb, Mesa's
# software Vulkan), with the repository mounted at /src and its own target dir in a volume.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y -o Acquire::Retries=5 --no-install-recommends ca-certificates curl xz-utils unzip git build-essential \
    pkg-config libclang-dev libasound2-dev libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev libx11-dev \
    libxcursor1 libxi6 libxrandr2 libx11-xcb1 xvfb mesa-vulkan-drivers libgl1-mesa-dri libva2 libva-drm2 libva-x11-2 libvdpau1 \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/target CARGO_INCREMENTAL=0
WORKDIR /src
