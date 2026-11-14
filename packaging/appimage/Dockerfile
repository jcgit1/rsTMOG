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
        libgtk-3-dev \
        libgtk-3-bin \
        libglib2.0-dev \
        libglib2.0-bin \
        libayatana-appindicator3-dev \
        libx11-dev \
        libxcursor-dev \
        libxi-dev \
        libxrandr-dev \
        libxkbcommon-dev \
        libwayland-dev \
        librsvg2-dev \
        gobject-introspection \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/appimage:${PATH}
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain 1.99.1 --profile minimal

# linuxdeploy and appimagetool are themselves AppImages. EXTRACT_AND_RUN
# lets this image pack a release without FUSE mounted in the container.
ENV APPIMAGE_EXTRACT_AND_RUN=1 \
    ARCH=x86_64 \
    DEPLOY_GTK_VERSION=3
RUN mkdir -p /opt/appimage \
    && curl -fsSL -o /opt/appimage/linuxdeploy \
        https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage \
    && curl -fsSL -o /opt/appimage/linuxdeploy-plugin-gtk \
        https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/3b67a1d1c1b0c8268f57f2bce40fe2d33d409cea/linuxdeploy-plugin-gtk.sh \
    && curl -fsSL -o /opt/appimage/appimagetool \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage \
    && chmod +x /opt/appimage/linuxdeploy /opt/appimage/linuxdeploy-plugin-gtk /opt/appimage/appimagetool

COPY entrypoint.sh /opt/appimage/entrypoint.sh
RUN chmod +x /opt/appimage/entrypoint.sh
ENTRYPOINT ["/opt/appimage/entrypoint.sh"]
