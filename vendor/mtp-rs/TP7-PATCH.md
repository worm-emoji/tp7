# TP-7 session-reuse patch

This is `mtp-rs` 0.13.3 with a small opt-in session policy used by the TP-7
CLI. The patch adds:

- `PtpSession::open_reusing_existing`
- `MtpDeviceBuilder::session_id`
- `MtpDeviceBuilder::reuse_existing_session`

Default `mtp-rs` behavior is unchanged. The TP-7 opts into reuse because its
firmware treats `CloseSession` as a request to leave MTP mode. FieldKit uses a
stable session ID and reuses the device-side session across app processes.
