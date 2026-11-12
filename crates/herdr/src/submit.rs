//! Submitting a line typed into a herdr pane: `tiller-panes`'s submit loop,
//! driven through [`Herdr`].

use tiller_panes::submit::{self, Target};
pub use tiller_panes::submit::{PRESSES, Pacing, READS};

use crate::client::{Herdr, HerdrError};
use crate::composer::Composer;

/// How a typed line's submit went.
pub type Submit = submit::Submit<HerdrError>;

/// herdr as a submit target.
struct Via<'a>(&'a dyn Herdr);

impl Target for Via<'_> {
    type Error = HerdrError;

    fn press_enter(&self, pane: &str) -> Result<(), HerdrError> {
        self.0.send_enter(pane)
    }

    fn read_visible(&self, pane: &str) -> Result<String, HerdrError> {
        self.0.read_visible(pane)
    }
}

/// One line being submitted (see [`submit::Submission`]).
#[derive(Debug)]
pub struct Submission(submit::Submission<HerdrError>);

impl Submission {
    /// Presses the first Enter.
    pub fn begin(herdr: &dyn Herdr, pane_id: &str) -> Result<Self, HerdrError> {
        submit::Submission::begin(&Via(herdr), pane_id).map(Self)
    }

    pub fn pane_id(&self) -> &str {
        self.0.pane_id()
    }

    /// One look, as [`submit::Submission::check`].
    pub fn check(
        &mut self,
        herdr: &dyn Herdr,
        left: bool,
        shows: impl Fn(&Composer) -> bool,
    ) -> Option<Submit> {
        self.0.check(&Via(herdr), left, shows)
    }
}

/// Presses Enter on a line already typed into `pane_id` and waits until it
/// has left the composer, as [`submit::submit`].
pub fn submit<E>(
    herdr: &dyn Herdr,
    pane_id: &str,
    pacing: Pacing,
    shows: impl Fn(&Composer) -> bool,
    left: impl FnMut() -> Result<bool, E>,
) -> Result<Submit, E> {
    submit::submit(&Via(herdr), pane_id, pacing, shows, left)
}
