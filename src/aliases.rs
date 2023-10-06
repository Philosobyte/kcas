/// A monotonically increasing counter used to identify a single KCAS operation.
///
/// The number of bits available for the sequence number depends on the length of a word on the
/// target platform as well as the maximum number of threads configured for any particular
/// [KCasState]. See [ThreadAndSequence] for more information.
pub(crate) type SequenceNumber = usize;

/// An identifier for a thread which performs multi-word CAS operations.
///
/// ThreadIds should be assigned incrementally starting from 1.
pub(crate) type ThreadId = usize;

pub(crate) type ThreadIndex = usize;

pub(crate) fn thread_id_to_thread_index(thread_id: ThreadId) -> ThreadIndex {
    thread_id - 1
}

pub(crate) fn thread_index_to_thread_id(thread_index: ThreadIndex) -> ThreadId {
    thread_index + 1
}

/// A usize which combines a [Stage] in the 3 most significant bits and a [SequenceNum] in the
/// remaining bits. This allows us to CAS both pieces of information in one operation.
pub(crate) type StageAndSequence = usize;


/// A usize which stores a [ThreadId] in its most significant bits and a [SequenceNumber] in the
/// remaining bits.
///
/// The number of bits taken up by `ThreadId` depends on the maximum number of threads specified
/// when creating a [KCasState] instance. For example, on x86-64, specifying a maximum
/// of 64 threads means  `ThreadId` takes up 6 bits, leaving 58 bits for the sequence number.
/// But specifying a maximum of 256 threads means `ThreadId` takes up 8 bits, leaving 56 bits for
/// the sequence number.
///
/// Luckily, only equality matters for sequence numbers, so as long as care is taken to wrap the
/// sequence number around, the number of bits allocated to the sequence number does not bound
/// the number of possible operations.
pub(crate) type ThreadAndSequence = usize;
