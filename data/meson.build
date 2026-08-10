# data/ subdirectory build configuration

gnome = import('gnome')
i18n = import('i18n')

# Install the compiled schema so the preferences dialog and the mail viewer
# use the same persistent settings in native and Flatpak installations.
gnome.compile_schemas(build_by_default: true)
gnome.post_install(glib_compile_schemas: true)

desktop_conf = configuration_data()
desktop_conf.set('APPLICATION_ID', application_id)

# Desktop file
desktop_file = i18n.merge_file(
  input: '@0@.desktop.in'.format(application_id),
  output: '@0@.desktop'.format(application_id),
  type: 'desktop',
  po_dir: meson.project_source_root() / 'po',
  install: true,
  install_dir: datadir / 'applications',
)

# Validate desktop file
desktop_file_validate = find_program('desktop-file-validate', required: false)
if desktop_file_validate.found()
  test('validate-desktop',
    desktop_file_validate,
    args: [desktop_file],
  )
endif

# AppStream metainfo
metainfo_file = i18n.merge_file(
  input: '@0@.metainfo.xml.in'.format(application_id),
  output: '@0@.metainfo.xml'.format(application_id),
  type: 'xml',
  po_dir: meson.project_source_root() / 'po',
  install: true,
  install_dir: datadir / 'metainfo',
)

# Validate metainfo
appstreamcli = find_program('appstreamcli', required: false)
if appstreamcli.found()
  test('validate-metainfo',
    appstreamcli,
    args: ['validate', '--no-net', metainfo_file],
  )
endif
