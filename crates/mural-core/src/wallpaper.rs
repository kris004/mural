use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use mural_ipc::{WallpaperAction, WallpaperEntry, WallpaperResponse};

use crate::config::MuralConfig;

const TAB: &str = "\t";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveOutput {
    pub name: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug)]
pub struct PreparedWallpaperChange {
    pub action: String,
    pub entries: Vec<WallpaperEntry>,
    pub selection: Vec<String>,
    canvas_before_start: Option<usize>,
    canvas_after_start: Option<usize>,
    layout_key: String,
    layout_before: LayoutState,
    layout: LayoutState,
    quarantine: Option<QuarantineMove>,
    shuffle_pos_before: usize,
    shuffle_bag_len_before: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanvasPreviewWindow {
    pub paths: Vec<String>,
    pub start_index: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuarantineMove {
    source: PathBuf,
    destination: PathBuf,
    moved: bool,
}

#[derive(Clone, Debug)]
pub struct WallpaperControl {
    wall_dir: PathBuf,
    state_dir: PathBuf,
    quarantine_dir: PathBuf,
    favorite_weight: usize,
    max_history: usize,
    library: Vec<String>,
    favorites: Vec<String>,
    shuffle_bag: Vec<String>,
    shuffle_pos: usize,
    layouts: BTreeMap<String, LayoutState>,
    rng: SimpleRng,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperPaths {
    pub wall_dir: PathBuf,
    pub state_dir: PathBuf,
    pub quarantine_dir: PathBuf,
}

pub fn configured_wallpaper_paths(config: &MuralConfig) -> Result<WallpaperPaths, String> {
    let home = home_dir()?;
    let wall_dir = env_path("MURAL_WALL_DIR")
        .or_else(|| config.wall_dir.clone())
        .or_else(|| env_path("WALL_DIR"))
        .unwrap_or_else(|| home.join("Pictures/wallpapers"));
    let state_dir = env_path("MURAL_STATE_DIR")
        .or_else(|| config.state_dir.clone())
        .unwrap_or_else(|| default_state_dir(env::var_os("XDG_STATE_HOME"), &home));
    let quarantine_dir = env_path("MURAL_QUARANTINE_DIR")
        .or_else(|| config.quarantine_dir.clone())
        .or_else(|| env_path("QUARANTINE_DIR"))
        .unwrap_or_else(|| wall_dir.join(".quarantine"));

    Ok(WallpaperPaths {
        wall_dir,
        state_dir,
        quarantine_dir,
    })
}

pub fn scan_top_level_library(paths: &WallpaperPaths) -> Result<Vec<String>, String> {
    scan_top_level_library_paths(&paths.wall_dir, &paths.quarantine_dir)
}

impl WallpaperControl {
    pub fn load(config: &MuralConfig) -> Result<Self, String> {
        let paths = configured_wallpaper_paths(config)?;
        let favorite_weight = env_usize("MURAL_FAVORITE_WEIGHT")
            .or(config.favorite_weight)
            .or_else(|| env_usize("FAVORITE_WEIGHT"))
            .unwrap_or(4)
            .max(1);
        let max_history = env_usize("MURAL_MAX_HISTORY")
            .or(config.max_history)
            .or_else(|| env_usize("MAX_HISTORY"))
            .unwrap_or(1000)
            .max(1);

        fs::create_dir_all(&paths.state_dir).map_err(|error| {
            format!(
                "failed to create mural state directory {}: {error}",
                paths.state_dir.display()
            )
        })?;

        let mut control = Self {
            wall_dir: paths.wall_dir,
            state_dir: paths.state_dir,
            quarantine_dir: paths.quarantine_dir,
            favorite_weight,
            max_history,
            library: Vec::new(),
            favorites: Vec::new(),
            shuffle_bag: Vec::new(),
            shuffle_pos: 0,
            layouts: BTreeMap::new(),
            rng: SimpleRng::seeded(),
        };
        control.load_persistent_lists();
        let _stats = control.rescan_top_level()?;
        if control.shuffle_bag.is_empty() && !control.library.is_empty() {
            control.rebuild_bag();
        }
        control.persist_core_state()?;
        Ok(control)
    }

    #[must_use]
    pub fn wall_dir(&self) -> &Path {
        &self.wall_dir
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    #[must_use]
    pub fn library_paths(&self) -> Vec<String> {
        self.library.clone()
    }

    #[must_use]
    pub fn upcoming_shuffle_paths(&self, count: usize) -> Vec<String> {
        self.peek_bag_forward(count)
    }

    #[must_use]
    pub fn preview_window(&self, required: &[String], tile_count: usize) -> Vec<String> {
        let target_len = tile_count.max(required.len()).max(1);
        let mut preview = self.peek_bag_window(target_len);

        for path in required {
            if path.is_empty() || preview.iter().any(|candidate| candidate == path) {
                continue;
            }
            if preview.len() < target_len {
                preview.push(path.clone());
            } else if let Some(slot) = preview.iter_mut().rev().find(|candidate| {
                !required
                    .iter()
                    .any(|required| required == candidate.as_str())
            }) {
                slot.clone_from(path);
            }
        }

        if preview.is_empty() {
            preview.extend(required.iter().filter(|path| !path.is_empty()).cloned());
        }
        preview.truncate(target_len);
        preview
    }

    pub fn canvas_preview_window_for_prepared_change(
        &self,
        prepared: &PreparedWallpaperChange,
        current: &[String],
        tile_count: usize,
    ) -> Result<CanvasPreviewWindow, String> {
        let tile_count = tile_count.max(1);
        let flat = prepared.layout.flattened_history();
        if flat.is_empty() {
            return Err(
                "canvas transition requires wallpaper history; run a wallpaper action first"
                    .to_owned(),
            );
        }

        let output_count = prepared.layout.out_count.max(1);
        let Some(before_start) = prepared.canvas_before_start else {
            return Err(
                "canvas transition requires captured wallpaper history positions; prepare the wallpaper action with a canvas transition"
                    .to_owned(),
            );
        };
        let Some(after_start) = prepared.canvas_after_start else {
            return Err(
                "canvas transition requires captured wallpaper history positions; prepare the wallpaper action with a canvas transition"
                    .to_owned(),
            );
        };

        let before_start = before_start.min(flat.len());
        let before_end = before_start.saturating_add(output_count).min(flat.len());
        let after_start = after_start.min(flat.len());
        let after_end = after_start.saturating_add(output_count).min(flat.len());

        let mut required_start = before_start.min(after_start);
        let mut required_end = before_end.max(after_end).max(required_start);
        let mut required_paths = Vec::new();

        for path in current.iter().filter(|path| !path.is_empty()) {
            let Some(index) = closest_path_index(&flat, path, before_start) else {
                return Err(format!(
                    "canvas transition requires the current wallpaper to be in mural history: {path}"
                ));
            };
            required_start = required_start.min(index);
            required_end = required_end.max(index.saturating_add(1));
            push_unique(&mut required_paths, path.clone());
        }

        for path in prepared.selection.iter().filter(|path| !path.is_empty()) {
            let Some(index) = closest_path_index(&flat, path, after_start) else {
                return Err(format!(
                    "canvas transition target is not in mural history: {path}"
                ));
            };
            required_start = required_start.min(index);
            required_end = required_end.max(index.saturating_add(1));
            push_unique(&mut required_paths, path.clone());
        }

        let required_len = required_end.saturating_sub(required_start);
        if required_len > tile_count {
            return Err(format!(
                "canvas transition needs {required_len} ordered history tile(s), but tile_count is {tile_count}; increase transition.canvas.tile_count or transition.canvas.max_tile_count"
            ));
        }

        let lead = (tile_count - required_len) / 2;
        let preview_start = required_start.saturating_sub(lead);
        let mut preview = Vec::with_capacity(tile_count);
        for path in flat.iter().skip(preview_start).take(tile_count).cloned() {
            preview.push(path);
        }

        let remaining = tile_count.saturating_sub(preview.len());
        preview.extend(self.peek_bag_forward(remaining));

        for path in required_paths {
            if !preview.iter().any(|candidate| candidate == &path) {
                return Err(format!(
                    "canvas transition could not fit required wallpaper in preview window: {path}; increase transition.canvas.tile_count or transition.canvas.max_tile_count"
                ));
            }
        }

        Ok(CanvasPreviewWindow {
            paths: preview,
            start_index: preview_start,
        })
    }

    pub fn add_top_level_file(&mut self, path: &Path) -> Result<bool, String> {
        if !self.is_eligible_top_level_wallpaper(path) {
            return Ok(false);
        }
        let path = normalize_path(path);
        if self.library.contains(&path) {
            return Ok(false);
        }
        self.library.push(path.clone());
        self.library.sort();
        self.shuffle_bag.push(path);
        self.persist_core_state()?;
        Ok(true)
    }

    pub fn rescan_response(&mut self) -> Result<WallpaperResponse, String> {
        let stats = self.rescan_top_level()?;
        let message = format!(
            "library\t{}\tadded\t{}\tremoved\t{}",
            self.library.len(),
            stats.added,
            stats.removed
        );
        Ok(WallpaperResponse {
            action: "rescan".to_owned(),
            message,
            entries: Vec::new(),
            favorites: Vec::new(),
        })
    }

    #[must_use]
    pub fn favorites_response(&self) -> WallpaperResponse {
        WallpaperResponse {
            action: "favorites".to_owned(),
            message: String::new(),
            entries: Vec::new(),
            favorites: self.favorites.clone(),
        }
    }

    pub fn current_response(
        &mut self,
        outputs: &[ActiveOutput],
    ) -> Result<WallpaperResponse, String> {
        let (layout_key, layout) = self.take_layout(outputs)?;
        let selection = match layout.current_selection(false) {
            Ok(selection) => selection,
            Err(error) => {
                self.layouts.insert(layout_key, layout);
                return Err(error);
            }
        };
        let entries = self.entries(outputs, &selection);
        self.layouts.insert(layout_key, layout);
        Ok(WallpaperResponse {
            action: "current".to_owned(),
            message: String::new(),
            entries,
            favorites: Vec::new(),
        })
    }

    pub fn favorite_action(
        &mut self,
        outputs: &[ActiveOutput],
        index: usize,
        favorite: bool,
    ) -> Result<WallpaperResponse, String> {
        validate_output_index(
            outputs,
            index,
            if favorite { "favorite" } else { "unfavorite" },
        )?;
        let (layout_key, mut layout) = self.take_layout(outputs)?;
        if let Err(error) = layout.current_selection(true) {
            self.layouts.insert(layout_key, layout);
            return Err(error);
        }
        if let Err(error) = self.ensure_window(&mut layout) {
            self.layouts.insert(layout_key, layout);
            return Err(error);
        }
        let selection = layout.get_window(layout.idx, layout.offset);
        let Some(target) = selection.get(index).cloned() else {
            return Err(format!("failed to locate wall at index {index}"));
        };

        if favorite {
            if !Path::new(&target).is_file() {
                self.layouts.insert(layout_key, layout);
                return Err(format!("favorite target missing: {target}"));
            }
            if !self.favorites.contains(&target) {
                self.favorites.push(target.clone());
                self.favorites.sort();
            }
        } else {
            self.favorites.retain(|wall| wall != &target);
        }
        self.rebuild_bag();
        layout.write()?;
        self.layouts.insert(layout_key, layout);
        self.persist_core_state()?;

        let action = if favorite { "favorite" } else { "unfavorite" };
        Ok(WallpaperResponse {
            action: action.to_owned(),
            message: format!("{action}\t{index}\t{target}"),
            entries: self.entries(outputs, &selection),
            favorites: Vec::new(),
        })
    }

    pub fn prepare_wallpaper_change(
        &mut self,
        action: &WallpaperAction,
        outputs: &[ActiveOutput],
        capture_canvas_positions: bool,
    ) -> Result<PreparedWallpaperChange, String> {
        if outputs.is_empty() {
            return Err("no active outputs".to_owned());
        }
        match action {
            WallpaperAction::Replace { index } => {
                validate_output_index(outputs, *index, "replace")?;
            }
            WallpaperAction::Quarantine { index } => {
                validate_output_index(outputs, *index, "quarantine")?;
            }
            _ => {}
        }
        let (layout_key, mut layout) = self.take_layout(outputs)?;
        let layout_before = layout.clone();
        let shuffle_pos_before = self.shuffle_pos;
        let shuffle_bag_len_before = self.shuffle_bag.len();
        let output_count = outputs.len();
        let mut quarantine = None;
        let canvas_before_start = if capture_canvas_positions {
            self.ensure_initialized(&mut layout)?;
            self.ensure_window(&mut layout)?;
            Some(layout.window_start())
        } else {
            None
        };

        let selection = match action {
            WallpaperAction::Next => self.run_next_back(&mut layout, false)?,
            WallpaperAction::Back => self.run_next_back(&mut layout, true)?,
            WallpaperAction::ShiftForward => self.run_shift(&mut layout, true)?,
            WallpaperAction::ShiftBack => self.run_shift(&mut layout, false)?,
            WallpaperAction::Replace { index } => self.run_replace(&mut layout, *index, None)?,
            WallpaperAction::Quarantine { index } => {
                self.ensure_initialized(&mut layout)?;
                self.ensure_window(&mut layout)?;
                let current = layout.get_window(layout.idx, layout.offset);
                let Some(target) = current.get(*index).cloned() else {
                    return Err(format!("quarantine failed to locate wall at index {index}"));
                };
                quarantine = self.prepare_quarantine_move(&target)?;
                self.run_replace(&mut layout, *index, Some(&target))?
            }
            _ => return Err(format!("{} does not render wallpapers", action.as_str())),
        };

        let selection = if selection.len() < output_count {
            self.ensure_window(&mut layout)?;
            layout.get_window(layout.idx, layout.offset)
        } else {
            selection
        };
        let selection = self.heal_selection(&mut layout, selection)?;
        let canvas_after_start = capture_canvas_positions.then(|| layout.window_start());
        let entries = self.entries(outputs, &selection);

        Ok(PreparedWallpaperChange {
            action: action.as_str().to_owned(),
            entries,
            selection,
            canvas_before_start,
            canvas_after_start,
            layout_key,
            layout_before,
            layout,
            quarantine,
            shuffle_pos_before,
            shuffle_bag_len_before,
        })
    }

    pub fn prepare_startup_display(
        &mut self,
        outputs: &[ActiveOutput],
    ) -> Result<PreparedWallpaperChange, String> {
        if outputs.is_empty() {
            return Err("no active outputs".to_owned());
        }
        let (layout_key, mut layout) = self.take_layout(outputs)?;
        let layout_before = layout.clone();
        let shuffle_pos_before = self.shuffle_pos;
        let shuffle_bag_len_before = self.shuffle_bag.len();
        self.ensure_initialized(&mut layout)?;
        self.ensure_window(&mut layout)?;
        let selection = layout.get_window(layout.idx, layout.offset);
        let selection = self.heal_selection(&mut layout, selection)?;
        let entries = self.entries(outputs, &selection);

        Ok(PreparedWallpaperChange {
            action: "startup".to_owned(),
            entries,
            selection,
            canvas_before_start: Some(layout.window_start()),
            canvas_after_start: Some(layout.window_start()),
            layout_key,
            layout_before,
            layout,
            quarantine: None,
            shuffle_pos_before,
            shuffle_bag_len_before,
        })
    }

    pub fn move_quarantine(
        &mut self,
        prepared: &mut PreparedWallpaperChange,
    ) -> Result<(), String> {
        let Some(move_plan) = &mut prepared.quarantine else {
            return Ok(());
        };
        if !move_plan.source.is_file() {
            return Ok(());
        }
        fs::create_dir_all(&self.quarantine_dir).map_err(|error| {
            format!(
                "failed to create quarantine directory {}: {error}",
                self.quarantine_dir.display()
            )
        })?;
        fs::rename(&move_plan.source, &move_plan.destination).map_err(|error| {
            format!(
                "failed to move {} to quarantine: {error}",
                move_plan.source.display()
            )
        })?;
        move_plan.moved = true;
        Ok(())
    }

    pub fn rollback_quarantine(prepared: &PreparedWallpaperChange) {
        let Some(move_plan) = &prepared.quarantine else {
            return;
        };
        if !move_plan.moved {
            return;
        }
        if let Err(error) = fs::rename(&move_plan.destination, &move_plan.source) {
            eprintln!(
                "murald: failed to roll back quarantine move {} -> {}: {error}",
                move_plan.destination.display(),
                move_plan.source.display()
            );
        }
    }

    pub fn rollback_wallpaper_change(&mut self, prepared: PreparedWallpaperChange) {
        Self::rollback_quarantine(&prepared);
        if self.shuffle_bag.len() == prepared.shuffle_bag_len_before {
            self.shuffle_pos = prepared.shuffle_pos_before.min(self.shuffle_bag.len());
        }
        self.layouts
            .insert(prepared.layout_key.clone(), prepared.layout_before);
    }

    pub fn commit_wallpaper_change(
        &mut self,
        prepared: PreparedWallpaperChange,
    ) -> Result<WallpaperResponse, String> {
        if let Some(move_plan) = &prepared.quarantine {
            let source = normalize_path(&move_plan.source);
            self.library.retain(|wall| wall != &source);
            self.favorites.retain(|wall| wall != &source);
            self.shuffle_bag.retain(|wall| wall != &source);
        }
        prepared.layout.write()?;
        self.layouts
            .insert(prepared.layout_key.clone(), prepared.layout);
        self.persist_core_state()?;
        Ok(WallpaperResponse {
            action: prepared.action,
            message: String::new(),
            entries: prepared.entries,
            favorites: Vec::new(),
        })
    }

    fn load_persistent_lists(&mut self) {
        self.library = read_lines(&self.library_path());
        self.favorites = read_lines(&self.favorites_path());
        self.shuffle_bag = read_lines(&self.bag_path());
        self.shuffle_pos = read_usize(&self.bag_pos_path(), 0);
    }

    fn rescan_top_level(&mut self) -> Result<RescanStats, String> {
        let old = self.library.iter().cloned().collect::<BTreeSet<_>>();
        let library = scan_top_level_library_paths(&self.wall_dir, &self.quarantine_dir)?;

        let new = library.iter().cloned().collect::<BTreeSet<_>>();
        let removed = old.difference(&new).count();
        let added = new.difference(&old).count();

        let added_paths = new.difference(&old).cloned().collect::<Vec<_>>();
        self.library = library;
        self.sanitize_favorites();
        let library_set = self.library.iter().collect::<BTreeSet<_>>();
        self.shuffle_bag
            .retain(|wall| library_set.contains(wall) && Path::new(wall).is_file());
        if self.shuffle_pos > self.shuffle_bag.len() {
            self.shuffle_pos = self.shuffle_bag.len();
        }
        let mut additions = added_paths;
        self.shuffle(&mut additions);
        self.shuffle_bag.extend(additions);
        if self.shuffle_bag.is_empty() && !self.library.is_empty() {
            self.rebuild_bag();
        }
        self.persist_core_state()?;
        Ok(RescanStats { added, removed })
    }

    fn is_eligible_top_level_wallpaper(&self, path: &Path) -> bool {
        if path.parent() != Some(self.wall_dir.as_path()) {
            return false;
        }
        if path.starts_with(&self.quarantine_dir) || !path.is_file() {
            return false;
        }
        is_supported_wallpaper(path)
    }

    fn sanitize_favorites(&mut self) {
        let library = self.library.iter().collect::<BTreeSet<_>>();
        self.favorites
            .retain(|wall| library.contains(wall) && Path::new(wall).is_file());
        self.favorites.sort();
        self.favorites.dedup();
    }

    fn rebuild_bag(&mut self) {
        self.sanitize_favorites();
        let favorites = self.favorites.iter().cloned().collect::<BTreeSet<_>>();
        let mut bag = Vec::new();
        for wall in &self.library {
            bag.push(wall.clone());
            if favorites.contains(wall) {
                for _ in 1..self.favorite_weight {
                    bag.push(wall.clone());
                }
            }
        }
        self.shuffle(&mut bag);
        self.shuffle_bag = bag;
        self.shuffle_pos = 0;
    }

    fn peek_bag_window(&self, count: usize) -> Vec<String> {
        if count == 0 {
            return Vec::new();
        }
        if self.shuffle_bag.is_empty() {
            return self.library.iter().take(count).cloned().collect();
        }

        let len = self.shuffle_bag.len();
        let before = count / 2;
        let start = self.shuffle_pos.saturating_sub(before).min(len);
        let mut preview = Vec::with_capacity(count.min(len));
        for offset in 0..count.min(len) {
            let index = (start + offset) % len;
            preview.push(self.shuffle_bag[index].clone());
        }
        preview
    }

    fn peek_bag_forward(&self, count: usize) -> Vec<String> {
        if count == 0 || self.shuffle_bag.is_empty() || self.shuffle_pos >= self.shuffle_bag.len() {
            return Vec::new();
        }

        self.shuffle_bag
            .iter()
            .skip(self.shuffle_pos)
            .take(count)
            .cloned()
            .collect()
    }

    fn bag_next_n(&mut self, count: usize, exclude: Option<&str>) -> Result<Vec<String>, String> {
        if self.shuffle_bag.is_empty() {
            self.rebuild_bag();
        }
        if self.shuffle_bag.is_empty() {
            return Err(format!(
                "no wallpapers found in {}",
                self.wall_dir.display()
            ));
        }

        let available_unique = self
            .library
            .iter()
            .filter(|wall| exclude != Some(wall.as_str()))
            .count();
        if available_unique == 0 && count > 0 {
            return Err(format!(
                "no replacement wallpapers found in {}",
                self.wall_dir.display()
            ));
        }
        let allow_duplicates = available_unique < count;
        let mut picked = Vec::with_capacity(count);
        let mut attempts = 0_usize;

        while picked.len() < count {
            if self.shuffle_pos >= self.shuffle_bag.len() {
                self.rebuild_bag();
                if self.shuffle_bag.is_empty() {
                    return Err(format!(
                        "no wallpapers found in {}",
                        self.wall_dir.display()
                    ));
                }
            }
            let wall = self.shuffle_bag[self.shuffle_pos].clone();
            self.shuffle_pos += 1;
            attempts = attempts.saturating_add(1);
            if exclude == Some(wall.as_str()) || !Path::new(&wall).is_file() {
                if attempts > self.shuffle_bag.len().saturating_mul(2).max(32) {
                    let _stats = self.rescan_top_level()?;
                    attempts = 0;
                }
                continue;
            }
            if !allow_duplicates && picked.contains(&wall) {
                continue;
            }
            picked.push(wall);
        }

        Ok(picked)
    }

    fn make_random_selection(&mut self, count: usize) -> Result<Vec<String>, String> {
        self.bag_next_n(count, None)
    }

    fn make_random_one(&mut self, exclude: Option<&str>) -> Result<String, String> {
        Ok(self.bag_next_n(1, exclude)?.remove(0))
    }

    fn ensure_initialized(&mut self, layout: &mut LayoutState) -> Result<(), String> {
        if layout.history.is_empty() {
            let selection = self.make_random_selection(layout.out_count)?;
            layout.append_set(selection);
            layout.idx = 1;
            layout.offset = 0;
        }
        Ok(())
    }

    fn ensure_window(&mut self, layout: &mut LayoutState) -> Result<(), String> {
        loop {
            let end = (layout.idx.saturating_sub(1))
                .saturating_mul(layout.out_count)
                .saturating_add(layout.offset)
                .saturating_add(layout.out_count);
            if layout.history.len().saturating_mul(layout.out_count) >= end {
                return Ok(());
            }
            let selection = self.make_random_selection(layout.out_count)?;
            layout.append_set(selection);
            if layout.idx < 1 {
                layout.idx = 1;
                layout.offset = 0;
            }
        }
    }

    fn run_next_back(
        &mut self,
        layout: &mut LayoutState,
        back: bool,
    ) -> Result<Vec<String>, String> {
        self.ensure_initialized(layout)?;
        if layout.idx < 1 {
            layout.idx = 1;
        }
        if back {
            if layout.idx > 1 {
                layout.idx -= 1;
            }
        } else if layout.idx < layout.history.len() {
            layout.idx += 1;
        } else {
            layout.idx += 1;
            self.ensure_window(layout)?;
        }
        Ok(layout.get_window(layout.idx, layout.offset))
    }

    fn run_shift(
        &mut self,
        layout: &mut LayoutState,
        forward: bool,
    ) -> Result<Vec<String>, String> {
        self.ensure_initialized(layout)?;
        if forward {
            layout.offset += 1;
            if layout.offset >= layout.out_count {
                layout.offset = 0;
                layout.idx += 1;
            }
            if layout.idx < 1 {
                layout.idx = 1;
                layout.offset = 0;
            }
            self.ensure_window(layout)?;
        } else if layout.offset > 0 {
            layout.offset -= 1;
        } else if layout.idx > 1 {
            layout.idx -= 1;
            layout.offset = layout.out_count - 1;
        }
        Ok(layout.get_window(layout.idx, layout.offset))
    }

    fn run_replace(
        &mut self,
        layout: &mut LayoutState,
        index: usize,
        exclude: Option<&str>,
    ) -> Result<Vec<String>, String> {
        self.ensure_initialized(layout)?;
        self.ensure_window(layout)?;
        let mut selection = layout.get_window(layout.idx, layout.offset);
        let replacement = self.make_random_one(exclude)?;
        if let Some(slot) = selection.get_mut(index) {
            *slot = replacement;
        }
        layout.trim_history();
        layout.history.push(selection.clone());
        layout.idx = layout.history.len();
        layout.offset = 0;
        layout.prune_history();
        Ok(selection)
    }

    fn heal_selection(
        &mut self,
        layout: &mut LayoutState,
        selection: Vec<String>,
    ) -> Result<Vec<String>, String> {
        let mut changed = false;
        let mut healed = Vec::with_capacity(selection.len());
        for wall in selection {
            if wall.is_empty() || Path::new(&wall).is_file() {
                healed.push(wall);
            } else {
                healed.push(self.make_random_one(None)?);
                changed = true;
            }
        }
        if changed {
            if layout.offset == 0 && (1..=layout.history.len()).contains(&layout.idx) {
                layout.history[layout.idx - 1].clone_from(&healed);
            } else {
                layout.trim_history();
                layout.history.push(healed.clone());
                layout.idx = layout.history.len();
                layout.offset = 0;
                layout.prune_history();
            }
        }
        Ok(healed)
    }

    fn prepare_quarantine_move(&self, target: &str) -> Result<Option<QuarantineMove>, String> {
        let source = PathBuf::from(target);
        if !source.is_file() {
            return Ok(None);
        }
        let file_name = source
            .file_name()
            .ok_or_else(|| format!("cannot quarantine path without file name: {target}"))?;
        let destination = unique_destination(&self.quarantine_dir, file_name);
        Ok(Some(QuarantineMove {
            source,
            destination,
            moved: false,
        }))
    }

    fn entries(&self, outputs: &[ActiveOutput], selection: &[String]) -> Vec<WallpaperEntry> {
        outputs
            .iter()
            .zip(selection.iter())
            .enumerate()
            .map(|(index, (output, wall))| WallpaperEntry {
                index,
                output: output.name.clone(),
                favorite: self.favorites.contains(wall),
                path: wall.clone(),
            })
            .collect()
    }

    fn take_layout(&mut self, outputs: &[ActiveOutput]) -> Result<(String, LayoutState), String> {
        if outputs.is_empty() {
            return Err("no active outputs".to_owned());
        }
        let names = outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>();
        let key = names.join("+");
        if let Some(layout) = self.layouts.remove(&key) {
            return Ok((key, layout));
        }
        let layout = LayoutState::load(
            self.state_dir.join(format!("layout-{key}")),
            outputs.len(),
            self.max_history,
        )?;
        Ok((key, layout))
    }

    fn library_path(&self) -> PathBuf {
        self.state_dir.join("library")
    }

    fn favorites_path(&self) -> PathBuf {
        self.state_dir.join("favorites")
    }

    fn bag_path(&self) -> PathBuf {
        self.state_dir.join("walls")
    }

    fn bag_pos_path(&self) -> PathBuf {
        self.state_dir.join("walls_idx")
    }

    fn persist_core_state(&self) -> Result<(), String> {
        atomic_write_lines(&self.library_path(), &self.library)?;
        atomic_write_lines(&self.favorites_path(), &self.favorites)?;
        atomic_write_lines(&self.bag_path(), &self.shuffle_bag)?;
        atomic_write_text(&self.bag_pos_path(), &format!("{}\n", self.shuffle_pos))?;
        Ok(())
    }

    fn shuffle<T>(&mut self, values: &mut [T]) {
        for index in (1..values.len()).rev() {
            let swap_with = self.rng.next_usize(index + 1);
            values.swap(index, swap_with);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LayoutState {
    layout_dir: PathBuf,
    out_count: usize,
    max_history: usize,
    history: Vec<Vec<String>>,
    idx: usize,
    offset: usize,
}

impl LayoutState {
    fn load(layout_dir: PathBuf, out_count: usize, max_history: usize) -> Result<Self, String> {
        fs::create_dir_all(&layout_dir).map_err(|error| {
            format!(
                "failed to create layout state directory {}: {error}",
                layout_dir.display()
            )
        })?;
        let history = read_lines(&layout_dir.join("history"))
            .into_iter()
            .filter(|line| !line.is_empty())
            .map(|line| line.split(TAB).map(ToOwned::to_owned).collect())
            .collect::<Vec<_>>();
        let mut idx = read_usize(&layout_dir.join("index"), 0);
        let mut offset = read_usize(&layout_dir.join("offset"), 0);
        if out_count > 0 {
            offset %= out_count;
        }
        if idx < 1 && !history.is_empty() {
            idx = 1;
        }
        Ok(Self {
            layout_dir,
            out_count,
            max_history,
            history,
            idx,
            offset,
        })
    }

    fn current_selection(&self, allow_extend: bool) -> Result<Vec<String>, String> {
        if self.history.is_empty() || self.idx < 1 {
            return Err("no current wallpaper history; run muralctl next first".to_owned());
        }
        let end = (self.idx - 1) * self.out_count + self.offset + self.out_count;
        if !allow_extend && self.history.len() * self.out_count < end {
            return Err("current wallpaper history is incomplete".to_owned());
        }
        Ok(self.get_window(self.idx, self.offset))
    }

    fn window_start(&self) -> usize {
        self.idx
            .saturating_sub(1)
            .saturating_mul(self.out_count)
            .saturating_add(self.offset)
    }

    fn flattened_history(&self) -> Vec<String> {
        self.history
            .iter()
            .flat_map(|selection| selection.iter().cloned())
            .collect()
    }

    fn get_window(&self, idx: usize, offset: usize) -> Vec<String> {
        let start = (idx.saturating_sub(1)) * self.out_count + offset;
        self.flattened_history()
            .into_iter()
            .skip(start)
            .take(self.out_count)
            .collect()
    }

    fn append_set(&mut self, selection: Vec<String>) {
        self.history.push(selection);
        self.prune_history();
    }

    fn trim_history(&mut self) {
        if self.history.len() > self.idx {
            self.history.truncate(self.idx);
        }
    }

    fn prune_history(&mut self) {
        if self.history.len() <= self.max_history {
            return;
        }
        let drop_count = self.history.len() - self.max_history;
        self.history.drain(0..drop_count);
        if self.idx > 0 {
            self.idx = self.idx.saturating_sub(drop_count).max(1);
        }
    }

    fn write(&self) -> Result<(), String> {
        let history = self
            .history
            .iter()
            .map(|selection| selection.join(TAB))
            .collect::<Vec<_>>();
        atomic_write_lines(&self.layout_dir.join("history"), &history)?;
        atomic_write_text(&self.layout_dir.join("index"), &format!("{}\n", self.idx))?;
        atomic_write_text(
            &self.layout_dir.join("offset"),
            &format!("{}\n", self.offset),
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn seeded() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_secs() ^ u64::from(duration.subsec_nanos()).rotate_left(32)
            });
        let pid = u64::from(std::process::id());
        Self {
            state: nanos ^ pid.rotate_left(17) ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }

    fn next_usize(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            0
        } else {
            let upper = u64::try_from(upper).unwrap_or(u64::MAX);
            usize::try_from(self.next_u64() % upper).unwrap_or(0)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RescanStats {
    added: usize,
    removed: usize,
}

fn validate_output_index(
    outputs: &[ActiveOutput],
    index: usize,
    label: &str,
) -> Result<(), String> {
    if index >= outputs.len() {
        return Err(format!(
            "{label} index out of range (0..{})",
            outputs.len().saturating_sub(1)
        ));
    }
    Ok(())
}

fn closest_path_index(paths: &[String], path: &str, anchor: usize) -> Option<usize> {
    paths
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.as_str() == path)
        .min_by_key(|(index, _)| index.abs_diff(anchor))
        .map(|(index, _)| index)
}

fn push_unique(paths: &mut Vec<String>, path: String) {
    if !paths.iter().any(|candidate| candidate == &path) {
        paths.push(path);
    }
}

fn is_supported_wallpaper(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp"
            )
        })
}

fn scan_top_level_library_paths(
    wall_dir: &Path,
    quarantine_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut library = Vec::new();
    if wall_dir.is_dir() {
        for entry in fs::read_dir(wall_dir).map_err(|error| {
            format!(
                "failed to read wallpaper directory {}: {error}",
                wall_dir.display()
            )
        })? {
            let entry = entry.map_err(|error| {
                format!(
                    "failed to read entry in wallpaper directory {}: {error}",
                    wall_dir.display()
                )
            })?;
            let path = entry.path();
            if is_eligible_top_level_wallpaper(wall_dir, quarantine_dir, &path) {
                library.push(normalize_path(&path));
            }
        }
    }
    library.sort();
    library.dedup();
    Ok(library)
}

fn is_eligible_top_level_wallpaper(wall_dir: &Path, quarantine_dir: &Path, path: &Path) -> bool {
    if path.parent() != Some(wall_dir) {
        return false;
    }
    if path.starts_with(quarantine_dir) || !path.is_file() {
        return false;
    }
    is_supported_wallpaper(path)
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn unique_destination(dir: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let initial = dir.join(file_name);
    if !initial.exists() {
        return initial;
    }
    let source = Path::new(file_name);
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("wallpaper");
    let extension = source.extension().and_then(|value| value.to_str());
    for suffix in 1.. {
        let name = match extension {
            Some(extension) => format!("{stem}.{suffix}.{extension}"),
            None => format!("{stem}.{suffix}"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search returns")
}

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())
}

fn default_state_dir(xdg_state_home: Option<OsString>, home: &Path) -> PathBuf {
    xdg_state_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map_or_else(
            || home.join(".local/state/mural"),
            |path| path.join("mural"),
        )
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(|value| PathBuf::from(value).expand_home())
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name).ok()?.parse().ok()
}

trait ExpandHome {
    fn expand_home(self) -> Self;
}

impl ExpandHome for PathBuf {
    fn expand_home(self) -> Self {
        let Some(raw) = self.to_str().map(ToOwned::to_owned) else {
            return self;
        };
        if raw == "~" {
            return home_dir().unwrap_or(self);
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            return home_dir().map_or(self, |home| home.join(rest));
        }
        self
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|content| content.lines().map(ToOwned::to_owned).collect())
        .unwrap_or_default()
}

fn read_usize(path: &Path, default: usize) -> usize {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| content.trim().parse().ok())
        .unwrap_or(default)
}

fn atomic_write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id()
    ));
    {
        let mut file = File::create(&tmp)
            .map_err(|error| format!("failed to create {}: {error}", tmp.display()))?;
        file.write_all(text.as_bytes())
            .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync {}: {error}", tmp.display()))?;
    }
    fs::rename(&tmp, path).map_err(|error| {
        format!(
            "failed to replace {} with {}: {error}",
            path.display(),
            tmp.display()
        )
    })
}

fn atomic_write_lines(path: &Path, lines: &[String]) -> Result<(), String> {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    atomic_write_text(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "mural-wallpaper-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn state_dir_ignores_empty_or_relative_xdg_home() {
        let home = Path::new("/home/test");
        let fallback = PathBuf::from("/home/test/.local/state/mural");

        for xdg_home in [Some(OsString::new()), Some(OsString::from("relative"))] {
            assert_eq!(default_state_dir(xdg_home, home), fallback);
        }
        assert_eq!(
            default_state_dir(Some(OsString::from("/tmp/xdg-state")), home),
            PathBuf::from("/tmp/xdg-state/mural")
        );
    }

    fn test_control(root: &Path) -> WallpaperControl {
        let wall_dir = root.join("walls");
        let state_dir = root.join("state");
        fs::create_dir_all(&wall_dir).unwrap();
        for name in ["a.jpg", "b.png", "c.webp", "d.jpeg"] {
            fs::write(wall_dir.join(name), b"not actually decoded").unwrap();
        }
        let mut control = WallpaperControl {
            wall_dir,
            state_dir,
            quarantine_dir: root.join("walls/.quarantine"),
            favorite_weight: 4,
            max_history: 1000,
            library: Vec::new(),
            favorites: Vec::new(),
            shuffle_bag: Vec::new(),
            shuffle_pos: 0,
            layouts: BTreeMap::new(),
            rng: SimpleRng { state: 1 },
        };
        fs::create_dir_all(&control.state_dir).unwrap();
        control.rescan_top_level().unwrap();
        control
    }

    fn outputs() -> Vec<ActiveOutput> {
        vec![
            ActiveOutput {
                name: "DP-1".to_owned(),
                x: 0,
                y: 0,
            },
            ActiveOutput {
                name: "DP-2".to_owned(),
                x: 100,
                y: 0,
            },
        ]
    }

    fn paths(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn prepared_for_canvas_preview(
        history: Vec<Vec<String>>,
        before_start: usize,
        after_start: usize,
        out_count: usize,
    ) -> PreparedWallpaperChange {
        let selection = history
            .iter()
            .flat_map(|row| row.iter().cloned())
            .skip(after_start)
            .take(out_count)
            .collect();
        PreparedWallpaperChange {
            action: "shift-back".to_owned(),
            entries: Vec::new(),
            selection,
            canvas_before_start: Some(before_start),
            canvas_after_start: Some(after_start),
            layout_key: "test".to_owned(),
            layout_before: LayoutState {
                layout_dir: PathBuf::new(),
                out_count,
                max_history: 1000,
                history: Vec::new(),
                idx: 0,
                offset: 0,
            },
            layout: LayoutState {
                layout_dir: PathBuf::new(),
                out_count,
                max_history: 1000,
                history,
                idx: after_start / out_count + 1,
                offset: after_start % out_count,
            },
            quarantine: None,
            shuffle_pos_before: 0,
            shuffle_bag_len_before: 0,
        }
    }

    #[test]
    fn indexes_only_top_level_supported_images() {
        let root = temp_state("top-level");
        let mut control = test_control(&root);
        fs::create_dir_all(control.wall_dir.join("nested")).unwrap();
        fs::write(control.wall_dir.join("nested/hidden.jpg"), b"x").unwrap();
        fs::write(control.wall_dir.join("note.txt"), b"x").unwrap();
        control.rescan_top_level().unwrap();
        assert_eq!(control.library.len(), 4);
    }

    #[test]
    fn new_top_level_file_is_added_to_bag() {
        let root = temp_state("new-file");
        let mut control = test_control(&root);
        let path = control.wall_dir.join("new.jpg");
        fs::write(&path, b"x").unwrap();
        assert!(control.add_top_level_file(&path).unwrap());
        assert!(control.library.iter().any(|wall| wall.ends_with("new.jpg")));
        assert!(
            control
                .shuffle_bag
                .iter()
                .any(|wall| wall.ends_with("new.jpg"))
        );
    }

    #[test]
    fn next_back_and_shift_use_flattened_history() {
        let root = temp_state("history");
        let mut control = test_control(&root);
        let first = control
            .prepare_wallpaper_change(&WallpaperAction::Next, &outputs(), false)
            .unwrap();
        let first_selection = first.selection.clone();
        control.commit_wallpaper_change(first).unwrap();
        let second = control
            .prepare_wallpaper_change(&WallpaperAction::Next, &outputs(), false)
            .unwrap();
        control.commit_wallpaper_change(second).unwrap();
        let back = control
            .prepare_wallpaper_change(&WallpaperAction::Back, &outputs(), false)
            .unwrap();
        assert_eq!(back.selection, first_selection);
        control.commit_wallpaper_change(back).unwrap();

        let shifted = control
            .prepare_wallpaper_change(&WallpaperAction::ShiftForward, &outputs(), false)
            .unwrap();
        assert_eq!(shifted.selection[0], first_selection[1]);
    }

    #[test]
    fn rollback_restores_staged_layout_and_shuffle_cursor() {
        let root = temp_state("rollback");
        let mut control = test_control(&root);
        let initial_shuffle_pos = control.shuffle_pos;

        let staged = control
            .prepare_wallpaper_change(&WallpaperAction::Next, &outputs(), false)
            .unwrap();
        assert!(control.shuffle_pos > initial_shuffle_pos);
        let staged_selection = staged.selection.clone();

        control.rollback_wallpaper_change(staged);
        assert_eq!(control.shuffle_pos, initial_shuffle_pos);

        let staged_again = control
            .prepare_wallpaper_change(&WallpaperAction::Next, &outputs(), false)
            .unwrap();
        assert_eq!(staged_again.selection, staged_selection);
    }

    #[test]
    fn startup_display_creates_or_restores_current_selection() {
        let root = temp_state("startup");
        let mut control = test_control(&root);
        let first = control.prepare_startup_display(&outputs()).unwrap();
        let first_selection = first.selection.clone();
        control.commit_wallpaper_change(first).unwrap();

        let restored = control.prepare_startup_display(&outputs()).unwrap();
        assert_eq!(restored.selection, first_selection);
    }

    #[test]
    fn preview_window_includes_required_paths() {
        let root = temp_state("preview");
        let control = test_control(&root);
        let required = vec![
            root.join("walls/a.jpg").to_string_lossy().into_owned(),
            root.join("walls/missing-but-current.jpg")
                .to_string_lossy()
                .into_owned(),
        ];

        let preview = control.preview_window(&required, 3);

        assert!(preview.iter().any(|path| path == &required[0]));
        assert!(preview.iter().any(|path| path == &required[1]));
        assert_eq!(preview.len(), 3);
    }

    #[test]
    fn upcoming_shuffle_paths_peeks_forward_from_current_cursor() {
        let root = temp_state("upcoming-shuffle");
        let mut control = test_control(&root);
        control.shuffle_bag = paths(&["bag-a", "bag-b", "bag-c"]);
        control.shuffle_pos = 1;

        assert_eq!(
            control.upcoming_shuffle_paths(2),
            paths(&["bag-b", "bag-c"])
        );
        assert!(control.upcoming_shuffle_paths(0).is_empty());
    }

    #[test]
    fn canvas_preview_uses_flattened_history_for_shift_back() {
        let root = temp_state("canvas-shift-back-preview");
        let mut control = test_control(&root);
        control.shuffle_bag = paths(&["bag-a", "bag-b"]);
        control.shuffle_pos = 0;
        let history = vec![
            paths(&["previous", "left", "middle"]),
            paths(&["right", "future-a", "future-b"]),
        ];
        let prepared = prepared_for_canvas_preview(history, 1, 0, 3);

        let preview = control
            .canvas_preview_window_for_prepared_change(
                &prepared,
                &paths(&["left", "middle", "right"]),
                5,
            )
            .unwrap();

        assert_eq!(preview.start_index, 0);
        assert_eq!(
            preview.paths,
            paths(&["previous", "left", "middle", "right", "future-a"])
        );
    }

    #[test]
    fn canvas_preview_preserves_forward_history_distance() {
        let root = temp_state("canvas-forward-preview");
        let mut control = test_control(&root);
        control.shuffle_bag = paths(&["bag-a", "bag-b"]);
        control.shuffle_pos = 0;
        let history = vec![
            paths(&["previous", "left", "middle"]),
            paths(&["right", "future-a", "future-b"]),
            paths(&["future-c", "future-d", "future-e"]),
        ];
        let prepared = prepared_for_canvas_preview(history, 1, 4, 3);

        let preview = control
            .canvas_preview_window_for_prepared_change(
                &prepared,
                &paths(&["left", "middle", "right"]),
                8,
            )
            .unwrap();

        assert_eq!(preview.start_index, 0);
        assert_eq!(
            preview.paths,
            paths(&[
                "previous", "left", "middle", "right", "future-a", "future-b", "future-c",
                "future-d"
            ])
        );
    }

    #[test]
    fn canvas_preview_fills_after_history_from_forward_bag_cursor() {
        let root = temp_state("canvas-forward-bag-fill");
        let mut control = test_control(&root);
        control.shuffle_bag = paths(&["bag-0", "bag-1", "bag-2", "bag-3"]);
        control.shuffle_pos = 2;
        let history = vec![paths(&["a", "b", "c"]), paths(&["d", "e", "f"])];
        let prepared = prepared_for_canvas_preview(history, 1, 2, 3);

        let preview = control
            .canvas_preview_window_for_prepared_change(&prepared, &paths(&["b", "c", "d"]), 8)
            .unwrap();

        assert_eq!(preview.start_index, 0);
        assert_eq!(
            preview.paths,
            paths(&["a", "b", "c", "d", "e", "f", "bag-2", "bag-3"])
        );
    }

    #[test]
    fn canvas_preview_errors_when_current_is_not_in_history() {
        let root = temp_state("canvas-current-missing");
        let control = test_control(&root);
        let history = vec![paths(&["a", "b", "c"]), paths(&["d", "e", "f"])];
        let prepared = prepared_for_canvas_preview(history, 1, 2, 3);

        let error = control
            .canvas_preview_window_for_prepared_change(&prepared, &paths(&["outside"]), 8)
            .unwrap_err();

        assert!(error.contains("current wallpaper"));
        assert!(error.contains("outside"));
    }
}
