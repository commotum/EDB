use super::*;

pub(super) fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native transport deadline elapsed",
        ))
    } else {
        Ok(remaining)
    }
}

pub(super) fn transfer(
    stream: &mut UnixStream,
    bytes: &mut [u8],
    deadline: Instant,
    write: bool,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let timeout = Some(remaining(deadline)?);
        let count = if write {
            stream.set_write_timeout(timeout)?;
            stream.write(&bytes[offset..])
        } else {
            stream.set_read_timeout(timeout)?;
            stream.read(&mut bytes[offset..])
        };
        match count {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "native endpoint disconnected",
                ));
            }
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) fn write_frame(
    stream: &mut UnixStream,
    bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    let mut length = (bytes.len() as u64).to_be_bytes();
    transfer(stream, &mut length, deadline, true)?;
    // Bounded frames, but avoid duplicating their allocation during writes.
    let mut offset = 0;
    while offset < bytes.len() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "native endpoint disconnected",
                ));
            }
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) fn read_frame(
    stream: &mut UnixStream,
    maximum: usize,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    let mut length = [0; 8];
    transfer(stream, &mut length, deadline, false)?;
    let length = usize::try_from(u64::from_be_bytes(length)).map_err(io::Error::other)?;
    if !(48..=maximum).contains(&length) {
        return Err(io::Error::other("invalid native frame size"));
    }
    let mut bytes = vec![0; length];
    transfer(stream, &mut bytes, deadline, false)?;
    Ok(bytes)
}

pub(super) fn unavailable(error: io::Error) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unavailable,
        "transport/unavailable",
        error.to_string(),
    )
}

pub(super) fn unknown(error: io::Error) -> SemanticError {
    SemanticError::new(
        ErrorCategory::UnknownOutcome,
        "transport/unknown-outcome",
        error.to_string(),
    )
}
