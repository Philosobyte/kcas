use core::fmt::{Display, Formatter};
use crate::err::{KCasError, OutOfBoundsStageError};

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
pub enum Stage {
    /// Indicates there is no KCAS operation in progress.
    Inactive = 0,
    /// Indicates a KCAS operation is in the middle of claiming ownership of the target addresses.
    Claiming = 1,
    /// Indicates a KCAS operation has already claimed all of its target addresses and is now
    /// setting the target addresses to the user-specified desired values.
    Setting = 2,
    /// Indicates a CAS failure occurred while claiming one of the target addresses. The KCAS
    /// operation is now "reverting" or changing all of its claimed target addresses back to their
    /// original values.
    Reverting = 3,
    /// Indicates one or more other threads have helped a given thread's KCAS operation to
    /// completion. A thread should never set its own stage to Successful.
    Successful = 4,
    /// Indicates one or more other threads finished reverting the KCAS operation. A thread will
    /// never set its own stage to Reverted.
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
    type Error = OutOfBoundsStageError;

    fn try_from(stage: usize) -> Result<Self, OutOfBoundsStageError> {
        match stage {
            i if i == Stage::Inactive as usize => Ok(Stage::Inactive),
            i if i == Stage::Claiming as usize => Ok(Stage::Claiming),
            i if i == Stage::Setting as usize => Ok(Stage::Setting),
            i if i == Stage::Reverting as usize => Ok(Stage::Reverting),
            i if i == Stage::Successful as usize => Ok(Stage::Successful),
            i if i == Stage::Reverted as usize => Ok(Stage::Reverted),
            i => Err(OutOfBoundsStageError(i))
        }
    }
}
