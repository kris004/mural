use mural_ipc::{Transition, WallpaperAction};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionKind {
    Next,
    Back,
    ShiftForward,
    ShiftBack,
    Replace,
    Quarantine,
    Startup,
}

impl ActionKind {
    pub(crate) const fn from_wallpaper_action(action: &WallpaperAction) -> Option<Self> {
        match action {
            WallpaperAction::Next => Some(Self::Next),
            WallpaperAction::Back => Some(Self::Back),
            WallpaperAction::ShiftForward => Some(Self::ShiftForward),
            WallpaperAction::ShiftBack => Some(Self::ShiftBack),
            WallpaperAction::Replace { .. } => Some(Self::Replace),
            WallpaperAction::Quarantine { .. } => Some(Self::Quarantine),
            WallpaperAction::Favorite { .. }
            | WallpaperAction::Unfavorite { .. }
            | WallpaperAction::Favorites
            | WallpaperAction::Current
            | WallpaperAction::Rescan => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActionProfile {
    pub(crate) transition: Transition,
}

impl ActionProfile {
    pub(crate) const fn new(transition: Transition) -> Self {
        Self { transition }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActionMap {
    next: ActionProfile,
    back: ActionProfile,
    shift_forward: ActionProfile,
    shift_back: ActionProfile,
    replace: ActionProfile,
    quarantine: ActionProfile,
    startup: ActionProfile,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActionTransitions {
    pub(crate) next: Transition,
    pub(crate) back: Transition,
    pub(crate) shift_forward: Transition,
    pub(crate) shift_back: Transition,
    pub(crate) replace: Transition,
    pub(crate) quarantine: Transition,
    pub(crate) startup: Transition,
}

impl ActionMap {
    pub(crate) const fn new(transitions: &ActionTransitions) -> Self {
        Self {
            next: ActionProfile::new(transitions.next),
            back: ActionProfile::new(transitions.back),
            shift_forward: ActionProfile::new(transitions.shift_forward),
            shift_back: ActionProfile::new(transitions.shift_back),
            replace: ActionProfile::new(transitions.replace),
            quarantine: ActionProfile::new(transitions.quarantine),
            startup: ActionProfile::new(transitions.startup),
        }
    }

    pub(crate) const fn profile(&self, kind: ActionKind) -> ActionProfile {
        match kind {
            ActionKind::Next => self.next,
            ActionKind::Back => self.back,
            ActionKind::ShiftForward => self.shift_forward,
            ActionKind::ShiftBack => self.shift_back,
            ActionKind::Replace => self.replace,
            ActionKind::Quarantine => self.quarantine,
            ActionKind::Startup => self.startup,
        }
    }

    pub(crate) fn transition_for_wallpaper_action(&self, action: &WallpaperAction) -> Transition {
        ActionKind::from_wallpaper_action(action)
            .map_or(Transition::Cut, |kind| self.profile(kind).transition)
    }

    pub(crate) const fn startup_transition(&self) -> Transition {
        self.profile(ActionKind::Startup).transition
    }

    pub(crate) const fn transitions(&self) -> [Transition; 7] {
        [
            self.next.transition,
            self.back.transition,
            self.shift_forward.transition,
            self.shift_back.transition,
            self.replace.transition,
            self.quarantine.transition,
            self.startup.transition,
        ]
    }
}
