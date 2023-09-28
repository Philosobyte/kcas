use core::fmt::{Display, Formatter};
use crate::err::{KCasError, InvalidStageError};

/// The stage of a thread's current multi-word CAS operation.
///
/// `Inactive` can transition to `Acquiring`.
/// `Acquiring` can transition to either `Setting` or `Reverting`.
///
/// A helper thread can transition a target thread's stage from `Setting` to `Successful`.
/// A helper thread can transition a target thread's stage from `Reverting` to `Reverted`
///
/// A target thread can change its own stage from `Setting` to `Inactive`.
/// A target thread can change its own stage from `Reverting` to `Inactive`.
/// A target thread can change its own stage from `Successful` to `Inactive`.
/// A target thread can change its own stage from `Reverted` to `Inactive`.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// There is no operation in progress. Only
    Inactive = 0,
    /// The thread is in the middle of acquiring ownership of the target addresses
    Acquiring = 1,
    /// The thread has already acquired all of its addresses and is now setting the target
    /// addresses to the desired elements
    Setting = 2,
    /// The thread has determined that the current value at a target address does not match the
    /// corresponding expected element, so the thread is rolling back all of its acquired markers
    Reverting = 3,
    /// One or more other threads have helped this thread's operation to completion.
    /// A thread will never set its own stage to Successful.
    Successful = 4,
    /// One or more other threads have determined this thread could not complete and have reverted
    /// all the operation's actions. A thread will never set its own stage to Reverted.
    Reverted = 5,
}

/// The total number of bits a [Stage] takes up. If Stage has 6 possible values, then a Stage
/// takes up 3 bits, because 6 can be represented in binary as 110, which is 3 bits long.
pub(crate) const STATUS_BIT_LENGTH: usize = 3;

impl Display for Stage {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl TryFrom<usize> for Stage {
    type Error = InvalidStageError;

    fn try_from(stage: usize) -> Result<Self, InvalidStageError> {
        match stage {
            i if i == Stage::Inactive as usize => Ok(Stage::Inactive),
            i if i == Stage::Acquiring as usize => Ok(Stage::Acquiring),
            i if i == Stage::Setting as usize => Ok(Stage::Setting),
            i if i == Stage::Reverting as usize => Ok(Stage::Reverting),
            i if i == Stage::Successful as usize => Ok(Stage::Successful),
            i if i == Stage::Reverted as usize => Ok(Stage::Reverted),
            i => Err(InvalidStageError::InvalidStage(i))
        }
    }
}
