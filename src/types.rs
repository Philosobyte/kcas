// a given thread.
///
/// The number of bits available for the sequence number depends on the length of a word on the
/// target platform as well as the maximum number of threads configured for any particular
/// [LuffyMcasThreadStates]. See [ThreadAndSequence] for more information.
pub(crate) type SequenceNumber = usize;

/// A number uniquely identifying a thread which performs multi-word CAS operations.
///
/// Threads need identifiers so they can help each other.
pub(crate) type ThreadId = usize;

/// A number which stores a [Stage] in its 3 most significant bits and a [SequenceNumber] in
/// the remaining bits. This combination allows us to CAS both pieces of data in one operation.
pub(crate) type StageAndSequence = usize;


/// A number which stores a [ThreadId] in its most significant bits and a [SequenceNumber] in the
/// remaining bits.
///
/// The number of bits taken up by `ThreadId` depends on the maximum number of threads specified
/// when creating a [LuffyMcasThreadStates] instance. For example, on x86-64, specifying a maximum
/// of 64 threads means  `ThreadId` takes up 6 bits, leaving 58 bits for the sequence number.
/// But specifying a maximum of 256 threads means `ThreadId` takes up 8 bits, leaving 56 bits for
/// the sequence number.
///
/// Luckily, only equality matters for sequence numbers, so as long as care is taken to wrap the
/// sequence number around, the number of bits allocated to the sequence number does not bound
/// the number of possible operations.
pub(crate) type ThreadAndSequence = usize;
