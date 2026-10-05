use std::{
    borrow::Cow,
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    hash::RandomState,
    io::Write,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
};

use blocking::unblock;
use eframe::egui::{self, InnerResponse};
use futures_lite::Future;
use ntsc_rs::NtscEffect;
use snafu::prelude::*;

use crate::path_compare::cmp_paths;

use super::{
    AppFn, NtscApp,
    error::{
        ApplicationError, CreatePresetFileSnafu, CreatePresetJSONSnafu,
        CreatePresetsDirectorySnafu, DeletePresetSnafu, FsSnafu, InstallPresetSnafu,
        JSONParseSnafu, JSONReadSnafu, RenamePresetSnafu,
    },
    executor::AppExecutor,
    layout_helper::LayoutHelper,
};

// TODO: drag and drop presets into folders

#[derive(Debug, Default)]
struct DirListingInner {
    state: DirState,
    /// Count the number of active FS operations occurring in this directory. Once this hits 0, we will refresh it.
    active_fs_operations: u32,
}

/// Listing of files inside a directory. Loading is done off-thread.
/// Contains the directory contents state (which may not be loaded), and the directory's path.
#[derive(Clone, Debug)]
struct DirListing(Arc<(Mutex<DirListingInner>, PathBuf)>);

impl PartialEq for DirListing {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug)]
enum DirEntry {
    File { path: PathBuf },
    Directory { listing: DirListing },
}

impl DirEntry {
    fn path(&self) -> &Path {
        match self {
            Self::File { path } => path.as_path(),
            Self::Directory { listing } => listing.path(),
        }
    }

    fn is_dir(&self) -> bool {
        matches!(&self, Self::Directory { .. })
    }
}

impl PartialOrd for DirEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DirEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .is_dir()
            .cmp(&self.is_dir())
            .then_with(|| cmp_paths(self.path(), other.path()))
    }
}

impl PartialEq for DirEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for DirEntry {}

#[derive(Debug, Default)]
enum DirState {
    #[default]
    /// This directory has not yet been loaded.
    NotLoaded,
    /// This directory is loading. There may be entries if this directory was previously loaded and is being reloaded to
    /// update it.
    Loading(Option<Vec<DirEntry>>),
    /// This directory failed to load due to an error.
    Error,
    /// This directory is loaded.
    Loaded(Vec<DirEntry>),
}

#[derive(Debug)]
struct SelectedPreset {
    path: PathBuf,
    settings: NtscEffect,
}

#[derive(Debug)]
enum Action {
    /// Load a preset from the given path and set it as the selected preset.
    LoadPreset { path: PathBuf },
    /// Save the current settings to a new path.
    CreatePreset { path: PathBuf, parent: DirListing },
    /// Overwrite the preset at the given path with the current settings.
    OverwritePreset { path: PathBuf, parent: DirListing },
    /// Rename or move a preset or directory.
    RenamePath {
        old_path: PathBuf,
        new_path: PathBuf,
        parent: DirListing,
    },
    /// Delete a preset file entirely.
    DeletePreset { path: PathBuf, parent: DirListing },
    /// Open the presets directory in the file manager.
    OpenPresetsDir { path: PathBuf },
    /// Explicitly reload the presets directory.
    ReloadPresetsDir { dir: DirListing },
}

#[derive(Debug, Default)]
pub struct PresetsListState {
    selected_preset: Option<SelectedPreset>,
    rename_path: Option<(PathBuf, String)>,
    new_preset_name: Option<(DirListing, String)>,
}

#[derive(Debug)]
pub struct PresetsState {
    presets_dir: DirListing,
    list_state: PresetsListState,
}

impl PresetsState {
    pub fn deselect_preset(&mut self) {
        self.list_state.selected_preset = None;
    }
}

impl Default for PresetsState {
    fn default() -> Self {
        Self {
            presets_dir: DirListing::new(NtscApp::presets_dir().unwrap_or_default()),
            list_state: Default::default(),
        }
    }
}

struct DirStateGuard<'a>(MutexGuard<'a, DirListingInner>);

impl Deref for DirStateGuard<'_> {
    type Target = DirState;

    fn deref(&self) -> &Self::Target {
        &self.0.state
    }
}

impl DerefMut for DirStateGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.state
    }
}

impl DirListing {
    fn new(path: PathBuf) -> Self {
        Self(Arc::new((Default::default(), path)))
    }
    fn path(&self) -> &Path {
        self.0.1.as_path()
    }

    fn lock(&self) -> DirStateGuard<'_> {
        DirStateGuard(self.0.0.lock().unwrap())
    }

    fn reload(&self, executor: &AppExecutor, create_path: bool) {
        let self_weak = Arc::downgrade(&self.0);
        let my_path = self.path().to_path_buf();
        executor.spawn(async move {
            // TODO: this needs to be inside the executor because we call reload when we already have the lock
            if let Some(inner) = self_weak.upgrade() {
                let mut inner = inner.0.lock().unwrap();
                inner.state = DirState::Loading(match std::mem::take(&mut inner.state) {
                    DirState::Loaded(presets) => Some(presets),
                    _ => None,
                });
            }
            if create_path {
                let my_path2 = my_path.clone();
                let res = unblock(move || fs::create_dir_all(my_path2))
                    .await
                    .context(CreatePresetsDirectorySnafu);
                if let Err(e) = res {
                    if let Some(listing) = self_weak.upgrade() {
                        listing.0.lock().unwrap().state = DirState::Error;
                    }
                    return Some(Box::new(move |_: &mut NtscApp| Err(e)) as _);
                }
            }
            let entries = match unblock(move || fs::read_dir(&my_path).context(FsSnafu)).await {
                Ok(entries) => entries,
                Err(err) => {
                    if let Some(listing) = self_weak.upgrade() {
                        let mut listing = listing.0.lock().unwrap();
                        listing.state = DirState::Error;
                    }
                    return Some(Box::new(move |_: &mut NtscApp| Err(err)) as _);
                }
            };

            let mut entries = entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let path = entry.path();
                    let file_type = entry.file_type().ok()?;
                    let is_directory = if file_type.is_symlink() {
                        fs::metadata(&path).ok()?.is_dir()
                    } else {
                        file_type.is_dir()
                    };

                    Some(if is_directory {
                        DirEntry::Directory {
                            listing: DirListing::new(path),
                        }
                    } else {
                        DirEntry::File { path }
                    })
                })
                .collect::<Vec<_>>();

            entries.sort();

            // Reload all child directory listings
            let mut listings_to_reload = Vec::new();

            if let Some(listing) = self_weak.upgrade() {
                let mut listing = listing.0.lock().unwrap();

                let old_state = std::mem::replace(&mut listing.state, DirState::Loaded(entries));

                // Copy over cached directory contents from the old state
                if let (
                    DirState::Loading(Some(old_entries)) | DirState::Loaded(old_entries),
                    DirState::Loaded(new_entries),
                ) = (old_state, &mut listing.state)
                {
                    for mut old_entry in old_entries {
                        if let Ok(new_entry_idx) = new_entries.binary_search(&old_entry) {
                            if let DirEntry::Directory { listing } = &mut old_entry {
                                let is_loaded = {
                                    let state = listing.lock();
                                    matches!(&*state, DirState::Loaded(_))
                                };
                                if is_loaded {
                                    listings_to_reload.push(listing.clone());
                                }
                            }

                            new_entries[new_entry_idx] = old_entry;
                        }
                    }
                }
            }

            if listings_to_reload.is_empty() {
                None
            } else {
                Some(Box::new(|app: &mut NtscApp| {
                    for listing in listings_to_reload {
                        listing.reload(&app.executor, false);
                    }

                    Ok(())
                }) as _)
            }
        })
    }

    fn do_fs_operation_then_refresh(
        &self,
        executor: &AppExecutor,
        op: impl Future<Output = Result<Option<AppFn>, ApplicationError>> + Send + 'static,
    ) {
        {
            let mut inner = self.0.0.lock().unwrap();
            inner.active_fs_operations += 1;
        }

        let self_weak = Arc::downgrade(&self.0);
        executor.spawn(async move {
            let res = op.await;

            Some(Box::new(move |app: &mut NtscApp| {
                match res {
                    Ok(Some(func)) => func(app)?,
                    Ok(None) => {}
                    Err(e) => app.handle_error(&e),
                }

                if let Some(inner) = self_weak.upgrade() {
                    let should_reload = {
                        let mut inner = inner.0.lock().unwrap();
                        inner.active_fs_operations -= 1;
                        inner.active_fs_operations == 0
                    };

                    if should_reload {
                        Self(inner).reload(&app.executor, false);
                    }
                }

                Ok(())
            }) as _)
        })
    }
}

impl PresetsListState {
    fn show_dir_listing(
        &mut self,
        ui: &mut egui::Ui,
        executor: &AppExecutor,
        dir: &DirListing,
        just_pressed_save: bool,
        selected_preset_modified: bool,
        is_root_listing: bool,
    ) -> Option<Action> {
        let mut action = None;

        let dir_state = dir.lock();
        let (is_loading, not_loaded) = {
            (
                matches!(&*dir_state, DirState::Loading(_)),
                matches!(&*dir_state, DirState::NotLoaded),
            )
        };

        if not_loaded {
            dir.reload(executor, is_root_listing);
        }
        match &*dir_state {
            DirState::NotLoaded | DirState::Loading(None) => {
                ui.add(egui::Spinner::new());
            }
            DirState::Error => {
                ui.label("Error loading presets directory");
            }
            DirState::Loaded(presets) | DirState::Loading(Some(presets)) => {
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                    if is_loading {
                        ui.disable();
                    }

                    for entry in presets.iter() {
                        match entry {
                            DirEntry::File { path } => {
                                if let Some(new_action) =
                                    self.show_preset_entry(ui, path, dir, selected_preset_modified)
                                {
                                    action = Some(new_action);
                                }
                            }
                            DirEntry::Directory { listing } => {
                                if let Some(new_action) = self.show_dir_entry(
                                    ui,
                                    executor,
                                    listing,
                                    dir,
                                    just_pressed_save,
                                    selected_preset_modified,
                                ) {
                                    action = Some(new_action);
                                }
                            }
                        }
                    }

                    let clear_preset_name = match &mut self.new_preset_name {
                        Some((new_preset_parent, new_preset_name)) if new_preset_parent == dir => {
                            ui.rtl(|ui| {
                                let close_button = ui.button("🗙");
                                let save_button = ui.button("Save");
                                let text_edit = ui.add_sized(
                                    ui.available_size(),
                                    egui::TextEdit::singleline(new_preset_name),
                                );
                                if just_pressed_save {
                                    text_edit.scroll_to_me(None);
                                    text_edit.request_focus();
                                }

                                let (esc_pressed, enter_pressed) = ui.input(|i| {
                                    (
                                        i.key_pressed(egui::Key::Escape),
                                        i.key_pressed(egui::Key::Enter),
                                    )
                                });
                                if (text_edit.lost_focus() && esc_pressed) || close_button.clicked()
                                {
                                    true
                                } else if (text_edit.lost_focus() && enter_pressed)
                                    || save_button.clicked()
                                {
                                    if !new_preset_name.is_empty() {
                                        let mut preset_path = NtscApp::presets_dir().unwrap();
                                        preset_path.push(new_preset_name);
                                        action = Some(Action::CreatePreset {
                                            path: preset_path,
                                            parent: dir.clone(),
                                        });
                                    }
                                    true
                                } else {
                                    false
                                }
                            })
                            .inner
                        }
                        _ => false,
                    };

                    if clear_preset_name {
                        self.new_preset_name = None;
                    }
                });
            }
        }

        action
    }

    fn show_dir_entry(
        &mut self,
        ui: &mut egui::Ui,
        executor: &AppExecutor,
        listing: &DirListing,
        parent: &DirListing,
        just_pressed_save: bool,
        selected_preset_modified: bool,
    ) -> Option<Action> {
        let id = ui.make_persistent_id(listing.path());
        let mut collapse_state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);

        let file_name = listing
            .path()
            .file_name()
            .unwrap_or(listing.path().as_os_str())
            .to_string_lossy();

        let label_name = format!(
            "{} {}",
            if collapse_state.is_open() {
                "📂"
            } else {
                "📁"
            },
            file_name
        );
        let InnerResponse {
            inner: mut action,
            response,
        } = self.show_entry_name(ui, listing.path(), parent, &label_name, false);
        if response.clicked() {
            collapse_state.toggle(ui);
        }

        collapse_state.show_body_indented(&response, ui, |ui| {
            if let Some(new_action) = self.show_dir_listing(
                ui,
                executor,
                listing,
                just_pressed_save,
                selected_preset_modified,
                false,
            ) {
                action = Some(new_action);
            }
        });

        action
    }

    fn show_preset_entry(
        &mut self,
        ui: &mut egui::Ui,
        preset_path: &Path,
        parent: &DirListing,
        selected_preset_modified: bool,
    ) -> Option<Action> {
        let selected = self
            .selected_preset
            .as_ref()
            .is_some_and(|selected_preset| selected_preset.path == preset_path);

        let mut file_name = preset_path
            .file_name()
            .unwrap_or(preset_path.as_os_str())
            .to_string_lossy();

        if selected && selected_preset_modified {
            file_name = Cow::Owned(format!("* {file_name}"));
        }

        let InnerResponse {
            inner: mut action,
            response,
        } = self.show_entry_name(ui, preset_path, parent, &file_name, selected);
        if response.clicked() {
            action = Some(Action::LoadPreset {
                path: preset_path.to_path_buf(),
            });
        }

        action
    }

    fn show_entry_name(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        parent: &DirListing,
        label: &str,
        selected: bool,
    ) -> InnerResponse<Option<Action>> {
        let mut action = None;
        let mut close_rename = false;

        let renamed_preset = &mut self.rename_path;
        let rename_name = renamed_preset.as_mut().and_then(|(renamed_path, name)| {
            if renamed_path == path {
                Some(name)
            } else {
                None
            }
        });

        let response = if let Some(name) = rename_name {
            let name_edit = ui.text_edit_singleline(name);

            if name_edit.lost_focus() {
                close_rename = true;
                if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    action = Some(Action::RenamePath {
                        old_path: path.to_owned(),
                        new_path: PathBuf::from_iter([
                            path.parent().unwrap().as_os_str(),
                            &OsString::from_str(name).unwrap(),
                        ]),
                        parent: parent.clone(),
                    });
                }
            }
            name_edit.request_focus();
            name_edit
        } else {
            let entry_label = ui.add(egui::Button::selectable(selected, label));

            entry_label.context_menu(|ui| {
                if ui.button("Delete").clicked() {
                    action = Some(Action::DeletePreset {
                        path: path.to_owned(),
                        parent: parent.clone(),
                    });
                    ui.close();
                }

                if ui.button("Rename").clicked() {
                    *renamed_preset = Some((
                        path.to_path_buf(),
                        path.file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy()
                            .into_owned(),
                    ));
                    ui.close();
                }
            });

            entry_label
        };

        if close_rename {
            self.rename_path = None;
        }

        InnerResponse {
            inner: action,
            response,
        }
    }
}

impl NtscApp {
    pub fn load_preset(&mut self, path: PathBuf) {
        self.spawn(async {
            let json_path = path.clone();
            let json = unblock(|| fs::read_to_string(json_path))
                .await
                .context(JSONReadSnafu);

            Some(Box::new(|app: &mut NtscApp| {
                let json = json?;
                let settings = app.settings_list.from_json(&json).context(JSONParseSnafu)?;
                app.set_effect_settings(settings.clone());
                app.presets_state.list_state.selected_preset =
                    Some(SelectedPreset { path, settings });
                Ok(())
            }) as _)
        });
    }

    fn presets_dir() -> Option<PathBuf> {
        let mut presets_dir = eframe::storage_dir(Self::APP_ID)?;
        presets_dir.push("presets");
        Some(presets_dir)
    }

    pub fn show_presets_pane(&mut self, ui: &mut egui::Ui) {
        let Some(presets_dir_path) = NtscApp::presets_dir() else {
            return;
        };

        let action = {
            let mut just_pressed_save = false;
            let mut action = None::<Action>;

            let selected_preset_modified = self
                .presets_state
                .list_state
                .selected_preset
                .as_ref()
                .is_some_and(|selected_preset| selected_preset.settings != self.effect_settings);

            {
                let presets_dir_state = self.presets_state.presets_dir.lock();

                ui.horizontal(|ui| {
                    if ui.button("Open folder").clicked() {
                        action = Some(Action::OpenPresetsDir {
                            path: presets_dir_path.clone(),
                        });
                    }
                    if ui.button("Reload").clicked() {
                        match &*presets_dir_state {
                            DirState::Loading(_) => {}
                            _ => {
                                action = Some(Action::ReloadPresetsDir {
                                    dir: self.presets_state.presets_dir.clone(),
                                });
                            }
                        }
                    }

                    if ui
                        .add_enabled(selected_preset_modified, egui::Button::new("Overwrite"))
                        .clicked()
                        && let Some(selected_preset) =
                            self.presets_state.list_state.selected_preset.as_ref()
                    {
                        action = Some(Action::OverwritePreset {
                            path: selected_preset.path.clone(),
                            parent: self.presets_state.presets_dir.clone(),
                        })
                    }

                    if ui.button("Save as").clicked() {
                        let mut preset_name = String::from("preset.json");
                        just_pressed_save = true;

                        let preset_names: Option<HashSet<&OsStr, RandomState>> =
                            if let DirState::Loaded(presets) | DirState::Loading(Some(presets)) =
                                &*presets_dir_state
                            {
                                Some(HashSet::from_iter(
                                    presets
                                        .iter()
                                        .filter_map(|preset| preset.path().file_name()),
                                ))
                            } else {
                                None
                            };

                        if let Some(preset_names) = preset_names {
                            let mut i = 1;
                            while preset_names.contains(
                                OsString::from_str(preset_name.as_str())
                                    .unwrap()
                                    .as_os_str(),
                            ) {
                                preset_name = format!("preset_{i}.json");
                                i += 1;
                            }
                        }

                        self.presets_state.list_state.new_preset_name =
                            Some((self.presets_state.presets_dir.clone(), preset_name));
                    }
                });
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .min_scrolled_height(0.0)
                .show(ui, |ui| {
                    let root_listing = self.presets_state.presets_dir.clone();
                    if let Some(new_action) = self.presets_state.list_state.show_dir_listing(
                        ui,
                        &self.executor,
                        &root_listing,
                        just_pressed_save,
                        selected_preset_modified,
                        true,
                    ) {
                        action = Some(new_action);
                    }
                });

            action
        };

        match action {
            Some(Action::LoadPreset { path }) => {
                self.load_preset(path);
            }
            Some(Action::RenamePath {
                old_path,
                new_path,
                parent,
            }) => {
                if new_path != old_path {
                    parent.do_fs_operation_then_refresh(
                        &self.executor,
                        unblock(|| {
                            if new_path.try_exists().context(RenamePresetSnafu)? {
                                return Err(ApplicationError::RenamePreset {
                                    source: std::io::Error::new(
                                        std::io::ErrorKind::AlreadyExists,
                                        "A file with that name already exists",
                                    ),
                                });
                            }
                            fs::rename(old_path, new_path).context(RenamePresetSnafu)?;

                            Ok(None)
                        }),
                    );
                }
            }
            Some(Action::CreatePreset { path, parent }) => {
                let new_selected_preset = SelectedPreset {
                    path,
                    settings: self.effect_settings.clone(),
                };
                match self
                    .settings_list
                    .to_json_string(&new_selected_preset.settings)
                    .context(CreatePresetJSONSnafu)
                {
                    Ok(json) => parent.do_fs_operation_then_refresh(&self.executor, async move {
                        if new_selected_preset
                            .path
                            .try_exists()
                            .context(CreatePresetFileSnafu)?
                        {
                            return Err(ApplicationError::CreatePresetFile {
                                source: std::io::Error::new(
                                    std::io::ErrorKind::AlreadyExists,
                                    "A file with that name already exists",
                                ),
                            });
                        }
                        let mut destination = fs::File::create(&new_selected_preset.path)
                            .context(CreatePresetFileSnafu)?;
                        destination
                            .write_all(json.as_bytes())
                            .context(CreatePresetFileSnafu)?;
                        Ok(Some(Box::new(|app: &mut NtscApp| {
                            app.presets_state.list_state.selected_preset =
                                Some(new_selected_preset);
                            Ok(())
                        }) as _))
                    }),
                    Err(e) => {
                        self.handle_error(&e);
                    }
                }
            }
            Some(Action::OverwritePreset { path, parent }) => {
                let new_selected_preset = SelectedPreset {
                    path,
                    settings: self.effect_settings.clone(),
                };
                match self
                    .settings_list
                    .to_json_string(&new_selected_preset.settings)
                    .context(CreatePresetJSONSnafu)
                {
                    Ok(json) => parent.do_fs_operation_then_refresh(&self.executor, async move {
                        let mut destination = fs::File::create(&new_selected_preset.path)
                            .context(CreatePresetFileSnafu)?;
                        destination
                            .write_all(json.as_bytes())
                            .context(CreatePresetFileSnafu)?;
                        Ok(Some(Box::new(|app: &mut NtscApp| {
                            app.presets_state.list_state.selected_preset =
                                Some(new_selected_preset);
                            Ok(())
                        }) as _))
                    }),
                    Err(e) => {
                        self.handle_error(&e);
                    }
                }
            }
            Some(Action::DeletePreset { path, parent }) => {
                parent.do_fs_operation_then_refresh(
                    &self.executor,
                    unblock(|| {
                        trash::delete(path).context(DeletePresetSnafu)?;
                        Ok(None)
                    }),
                );
            }

            Some(Action::OpenPresetsDir { path }) => {
                self.handle_result(open::that_detached(&path));
            }
            Some(Action::ReloadPresetsDir { dir }) => {
                dir.reload(&self.executor, true);
            }
            _ => {}
        }
    }

    pub fn install_presets<I: IntoIterator<Item = PathBuf> + Send + 'static>(&mut self, presets: I)
    where
        <I as IntoIterator>::IntoIter: Send,
    {
        let presets = presets.into_iter();
        self.presets_state
            .presets_dir
            .do_fs_operation_then_refresh(&self.executor, async move {
                for preset in presets {
                    let Some(file_name) = preset.file_name() else {
                        continue;
                    };
                    let mut dst_path = Self::presets_dir().unwrap();
                    dst_path.push(file_name);
                    unblock(|| fs::copy(preset, dst_path))
                        .await
                        .context(InstallPresetSnafu)?;
                }

                Ok(None)
            });
    }
}
