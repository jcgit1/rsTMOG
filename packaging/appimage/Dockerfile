# Toolchain image only. The source tree is mounted at /src by build.sh so
# a code change does not rebuild Ubuntu and Rust.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        file \
        pkg-config \
        patchelf \
        libx11-dev \
        libxcursor-dev \
        libxi-dev \
        libxrandr-dev \
        libxkbcommon-dev \
        libwayland-dev \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/appimage:${PATH}
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain 1.99.1 --profile minimal

# linuxdeploy and appimagetool are themselves AppImages. EXTRACT_AND_RUN
# lets this image pack a release without FUSE mounted in the container.
ENV APPIMAGE_EXTRACT_AND_RUN=1 \
    ARCH=x86_64
# Pinned releases, SHA-256 verified (see tools.sh).
COPY tools.sh /opt/appimage/tools.sh
RUN bash /opt/appimage/tools.sh /opt/appimage
ENV APPIMAGE_RUNTIME=/opt/appimage/runtime-x86_64

COPY entrypoint.sh /opt/appimage/entrypoint.sh
RUN chmod +x /opt/appimage/entrypoint.sh
ENTRYPOINT ["/opt/appimage/entrypoint.sh"]
