# Meson build wrapper for viaduct.
#
# This is a thin orchestration layer around `cargo build --release`. The Rust
# workspace at `viaduct/` and `viaduct-core/` is the source of truth — meson
# only handles the GNOME-shaped install layout (binary into `$bindir`, gschema
# into `$datadir/glib-2.0/schemas`, desktop / metainfo into their canonical
# subdirs) so the project ships cleanly through Flatpak and any distro that
# expects a meson buildsystem.
#
# Use:
#   meson setup builddir --prefix=/usr
#   meson compile -C builddir
#   meson install -C builddir
#
# The Flatpak manifest at `org.virinvictus.Viaduct.json` invokes meson the
# same way against `/app`.

project(
  'viaduct',
  'rust',
  version: '1.8.0',
  meson_version: '>= 0.59.0',
  license: 'MIT',
)

application_id = 'org.virinvictus.Viaduct'
prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')

cargo = find_program('cargo')
glib_compile_schemas = find_program('glib-compile-schemas')

# Cargo build target. Build always runs (cargo handles its own incremental
# logic) so meson stays out of Rust's dependency tracking.
cargo_options = ['--manifest-path', meson.project_source_root() / 'Cargo.toml']
cargo_options += ['--release']
cargo_options += ['--package', 'viaduct']

cargo_env = environment()
cargo_env.set('CARGO_TARGET_DIR', meson.project_build_root() / 'cargo-target')

cargo_build = custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: 'viaduct',
  console: true,
  install: true,
  install_dir: bindir,
  command: [
    'env', 'CARGO_TARGET_DIR=' + meson.project_build_root() / 'cargo-target',
    cargo, 'build', cargo_options,
    '&&', 'cp', meson.project_build_root() / 'cargo-target' / 'release' / 'viaduct', '@OUTPUT@',
  ],
)

# GSettings schema. install_data drops the .xml into the canonical schema
# directory; meson.add_install_script invokes glib-compile-schemas at install
# time so end users don't need a post-install hook.
install_data(
  'data/' + application_id + '.gschema.xml',
  install_dir: datadir / 'glib-2.0' / 'schemas',
)

meson.add_install_script(
  glib_compile_schemas,
  datadir / 'glib-2.0' / 'schemas',
)

# Article themes. Bundled into the binary via include_str! at compile time,
# but we also ship the source tree so packagers and curious users can read
# the CSS without unpacking the binary.
install_subdir(
  'data/themes',
  install_dir: datadir / 'viaduct',
)

# Desktop entry + AppStream metainfo. Desktop file goes into
# $datadir/applications, metainfo into $datadir/metainfo (the AppStream-spec
# location, replacing the older $datadir/appdata path the file is named for
# historically).
install_data(
  'data/' + application_id + '.desktop',
  install_dir: datadir / 'applications',
)

install_data(
  'data/' + application_id + '.appdata.xml',
  install_dir: datadir / 'metainfo',
  rename: application_id + '.metainfo.xml',
)

# Application icon. Scalable SVG covers every desktop-environment size; the
# symbolic SVG is for places GTK draws monochrome glyphs (sidebars, menus,
# tray indicators). hicolor is the freedesktop fallback theme that every
# real icon theme inherits from, so installing here makes the icon available
# regardless of the user's active theme.
install_data(
  'data/icons/hicolor/scalable/apps/' + application_id + '.svg',
  install_dir: datadir / 'icons' / 'hicolor' / 'scalable' / 'apps',
)
install_data(
  'data/icons/hicolor/symbolic/apps/' + application_id + '-symbolic.svg',
  install_dir: datadir / 'icons' / 'hicolor' / 'symbolic' / 'apps',
)

summary({
  'prefix': prefix,
  'bindir': bindir,
  'datadir': datadir,
  'application id': application_id,
}, section: 'Configuration')
