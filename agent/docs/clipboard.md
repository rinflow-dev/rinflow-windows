# Windows clipboard and on-demand files

The sharing switch still controls both clipboard directions. Text, HTML and
images keep the existing format conversions and 8 MiB clipboard limit.

## Native file copies

Windows enables `CoreConfig::on_demand_files`. When the peer advertises
`CAPABILITY_FILE_STREAMS` (bit 12), the core publishes `files_available` with
a validated `FileManifest`. It does not create a download or write a cache.

The clipboard STA installs an OLE `IDataObject` and keeps pumping messages.
`FileGroupDescriptorW` contains names, directories and 64-bit sizes.
`FileContents` provides a read-only `IStream` for each file. QueryGetData,
descriptor retrieval, Stat, Seek, Clone and obtaining a stream do not read
file contents. The first Read acquires an authenticated source lease.

Each network read requests at most 256 KiB. There are at most 16 outstanding
reads and 16 leases on either side of a core. Source disk work and queued
responses share a 16-permit worker budget; no whole-file buffer is used.
The target controls the reading rate, so a slow destination applies backpressure.
Offsets and sizes are u64, including files larger than 4 GiB. Read beyond
EOF behaves like a read-only file stream, and Clone has an independent cursor.

The source pins the offer for an operation. Copying new text/files on either
device replaces the clipboard offer but does not cancel that operation.
Completing a stream never writes a new clipboard. Unstarted old offers are
revoked, including when the new clipboard is empty or private.

Explorer decides the final destination, collisions and its copy progress.
`IDataObjectAsyncCapability` lets compatible targets negotiate background
extraction. EndOperation reports the target's result; without that callback,
the core reports only content delivery/release, not destination durability.
The source's sending progress does not count as receiver success.

The transfer card's Cancel action revokes the corresponding lease. Turning
sharing off revokes all file leases and pending scans. Reads also fail when
the connection closes, the source changes, or a read exceeds 15 seconds.
An operation idle for 15 minutes expires. A cancelled stream does not silently
restart. The target's next new asynchronous operation can try again.

## Older peers

Windows receives files only through file streams (capability bit 12). A
peer without it offers files in the original FileRequest/FileChunk
protocol; Windows declines that offer at once (`files_offered` →
`cancel_files`), downloads nothing, shows no transfer card, and tells the
user once per run to update LinkCursor on that device. A peer's
`text/uri-list` is never written to the clipboard as files.

## Source enumeration and compatibility

Source directory enumeration runs on a blocking worker with cancellation
checks and snapshot-generation validation. A newer clipboard invalidates
the scan. Entry/depth limits remain 10,000 / 32, and a directory listing is
bounded before sorting. Enumeration errors are reported rather than silently
omitting files. No source absolute path is put on the wire.

The Windows provider rejects reserved/invalid names, case-insensitive aliases
and descriptor paths of 260 UTF-16 units or more; it does not truncate names.
Targets must understand Shell virtual files. Programs accepting only CF_HDROP
are not yet a transparent destination for these offers. Cross-machine Cut
never deletes source files. File modification checks detect changes before
and after each read; they are not an immutable filesystem snapshot.

These streams seek within a live connection. Persistent reconnect/resume,
cross-device file relaying, deferred large-image representations and cross-screen
drag initiation are not implemented by this change. macOS app sources are unchanged;
its existing receive mode and protocol remain supported.

## Validation

Core tests cover protocol limits, no eager download, operation leases across
clipboard replacement, changed source rejection, sparse-file offsets above
4 GiB, and a slow consumer retaining the receive budget.

The Windows tests exercise FILEDESCRIPTORW sizes/names and copy actual
IStream contents into Windows file-storage streams, using two encrypted
cores, multiple files, an empty file and an empty directory. The test also
checks metadata-only access, Seek/Clone and replacing both clipboards during
an active stream. The default run does not replace the user's system clipboard.

A manual Explorer paste smoke test on 2026-10-27 successfully materialized
the same files/folders directly in its destination. To reproduce with the
test harness, opt in with `LINKCURSOR_TEST_CLIPBOARD=1` and set
`LINKCURSOR_TEST_DESTINATION_FILE` to a writable path receiving the test's
destination path. Open that directory and paste within 120 seconds. The
harness restores the previous clipboard if the test object still owns it.

Run `cargo test` in both `linkcursor-core` and `linkcursor-windows/agent`, then
build the WinUI app with `linkcursor-windows/build.ps1`.
