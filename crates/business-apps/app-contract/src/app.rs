//! The `BusinessApp` plugin contract and the installed-app registry.

/// Installation metadata for one user-facing app, as plain data.
///
/// Associated constants are not object-safe, so a `BusinessApp` cannot be
/// stored as `dyn`. Instead apps are reduced to this value type at push time
/// and the registry stores a plain `Vec<AppMeta>` — no vtables needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppMeta {
    /// Canonical id, e.g. `"media"` — used as the home-screen key.
    pub id: &'static str,
    pub name: &'static str,
    /// Iconify glyph name, e.g. `"lucide:film"`.
    pub icon: &'static str,
    pub group: &'static str,
    /// Home page path, e.g. `"/media-library"`.
    pub home: &'static str,
    pub description: &'static str,
}

/// The contract every installable business app implements, for an empty
/// marker struct in its own crate. Not `dyn`-compatible (associated consts);
/// a `meta()` snapshot is taken when the app registers itself.
pub trait BusinessApp {
    const ID: &'static str;
    const NAME: &'static str;
    const ICON: &'static str;
    const GROUP: &'static str;
    const HOME: &'static str;
    const DESCRIPTION: &'static str;

    /// Snapshot of this concrete app's constants.
    fn meta(&self) -> AppMeta {
        AppMeta {
            id: Self::ID,
            name: Self::NAME,
            icon: Self::ICON,
            group: Self::GROUP,
            home: Self::HOME,
            description: Self::DESCRIPTION,
        }
    }
}

/// The registry of installed apps. Populated at link time by the `apps/lan`
/// binary — wiring only, no dynamic linking.
#[derive(Default)]
pub struct AppRegistry {
    apps: Vec<AppMeta>,
}

impl AppRegistry {
    pub fn new() -> Self {
        Self { apps: Vec::new() }
    }

    /// Register one installed app by its marker type, e.g.
    /// `registry.push(media::Media)`. Apps are kept in push order, which is
    /// the order shown on the home screen.
    pub fn push(&mut self, app: impl BusinessApp + 'static) -> &mut Self {
        self.apps.push(app.meta());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    pub fn by_id(&self, id: &str) -> Option<AppMeta> {
        self.apps.iter().copied().find(|app| app.id == id)
    }

    pub fn all(&self) -> &[AppMeta] {
        &self.apps
    }

    /// Metadata for every installed app, in registration order.
    pub fn meta(&self) -> Vec<AppMeta> {
        self.apps.clone()
    }

    pub fn meta_by_id(&self, id: &str) -> Option<AppMeta> {
        self.by_id(id)
    }
}