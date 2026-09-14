use super::*;

pub(super) struct DeadlineStream {
    pub(super) socket: TcpStream,
    pub(super) deadline: Instant,
}
impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.socket
            .set_read_timeout(Some(remaining(self.deadline)?))?;
        self.socket.read(bytes)
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket
            .set_write_timeout(Some(remaining(self.deadline)?))?;
        self.socket.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket
            .set_write_timeout(Some(remaining(self.deadline)?))?;
        self.socket.flush()
    }
}
pub(super) fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(&(bytes.len() as u64).to_be_bytes())?;
    stream.write_all(bytes)
}
pub(super) fn read_frame(stream: &mut impl Read, maximum: usize) -> io::Result<Vec<u8>> {
    let mut length = [0; 8];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u64::from_be_bytes(length)).map_err(io::Error::other)?;
    if length > maximum {
        return Err(io::Error::other("remote frame exceeds configured limit"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub(super) fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "remote deadline elapsed",
        ))
    } else {
        Ok(left)
    }
}
pub(super) fn invalid(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::incorrect(code, message)
}
pub(super) fn unavailable(error: io::Error) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unavailable,
        "remote/unavailable",
        error.to_string(),
    )
}
pub(super) fn unknown(error: io::Error) -> SemanticError {
    SemanticError::new(
        ErrorCategory::UnknownOutcome,
        "remote/unknown-outcome",
        error.to_string(),
    )
}
pub(super) fn tls_error(_: native_tls::Error) -> SemanticError {
    invalid(
        "remote/tls-config",
        "TLS identity or trust configuration is invalid",
    )
}
pub(super) const ENDPOINT_MAGIC: &[u8; 5] = b"ATRE\x01";
pub(super) const MAX_ENDPOINT_NAME: usize = 64 * 1024;
pub(super) const MAX_ENDPOINT_BYTES: usize = 5 + 4 + 8 + 32 + 5 * (4 + MAX_ENDPOINT_NAME);

pub(super) fn endpoint_key(identity: &DatabaseIdentity) -> String {
    format!("remote-writers/{}", identity.database_id())
}

pub(super) fn no_writer() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unavailable,
        "remote/no-writer",
        "No published endpoint has a current writer lease for this database",
    )
}

pub(super) fn encode_endpoint(endpoint: &RemoteWriterEndpoint) -> Result<Vec<u8>, SemanticError> {
    validate_route(endpoint.address, &endpoint.tls_server_name)?;
    let address = endpoint.address.to_string();
    let fields = [
        endpoint.identity.database_id(),
        endpoint.identity.lineage_id(),
        endpoint.holder_id.as_str(),
        address.as_str(),
        endpoint.tls_server_name.as_str(),
    ];
    if fields.iter().any(|field| field.len() > MAX_ENDPOINT_NAME) {
        return Err(invalid(
            "remote/identity-capacity",
            "Endpoint identity metadata exceeds capacity",
        ));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(ENDPOINT_MAGIC);
    bytes.extend_from_slice(&PROTOCOL.to_be_bytes());
    bytes.extend_from_slice(&endpoint.lease_epoch.to_be_bytes());
    bytes.extend_from_slice(&endpoint.instance_id);
    for field in fields {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }
    Ok(bytes)
}

pub(super) fn decode_endpoint(bytes: &[u8]) -> Result<RemoteWriterEndpoint, SemanticError> {
    pub(super) fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Result<&'a [u8], SemanticError> {
        if n > bytes.len() {
            return Err(invalid("remote/discovery", "Truncated endpoint descriptor"));
        }
        let (head, tail) = bytes.split_at(n);
        *bytes = tail;
        Ok(head)
    }
    pub(super) fn field<'a>(bytes: &mut &'a [u8]) -> Result<&'a str, SemanticError> {
        let n = u32::from_be_bytes(take(bytes, 4)?.try_into().unwrap()) as usize;
        if n > MAX_ENDPOINT_NAME {
            return Err(invalid(
                "remote/discovery",
                "Endpoint field exceeds capacity",
            ));
        }
        std::str::from_utf8(take(bytes, n)?)
            .map_err(|_| invalid("remote/discovery", "Endpoint field is not UTF-8"))
    }
    if bytes.len() > MAX_ENDPOINT_BYTES {
        return Err(invalid(
            "remote/discovery",
            "Endpoint descriptor exceeds capacity",
        ));
    }
    let mut bytes = bytes;
    if take(&mut bytes, ENDPOINT_MAGIC.len())? != ENDPOINT_MAGIC
        || u32::from_be_bytes(take(&mut bytes, 4)?.try_into().unwrap()) != PROTOCOL
    {
        return Err(invalid(
            "remote/version",
            "Endpoint descriptor version is unsupported",
        ));
    }
    let lease_epoch = u64::from_be_bytes(take(&mut bytes, 8)?.try_into().unwrap());
    let instance_id = take(&mut bytes, 32)?.try_into().unwrap();
    let database = field(&mut bytes)?;
    let lineage = field(&mut bytes)?;
    let holder = field(&mut bytes)?;
    let address = field(&mut bytes)?.parse().map_err(|_| {
        invalid(
            "remote/discovery",
            "Endpoint address is not a numeric TCP socket address",
        )
    })?;
    let name = field(&mut bytes)?;
    if !bytes.is_empty() || lease_epoch == 0 {
        return Err(invalid(
            "remote/discovery",
            "Invalid endpoint framing or epoch",
        ));
    }
    validate_route(address, name)?;
    Ok(RemoteWriterEndpoint {
        identity: DatabaseIdentity::new(database, lineage),
        address,
        tls_server_name: name.into(),
        holder_id: holder.into(),
        lease_epoch,
        instance_id,
    })
}
pub(super) fn validate_route(address: SocketAddr, name: &str) -> Result<(), SemanticError> {
    if address.port() == 0
        || address.ip().is_unspecified()
        || address.ip().is_multicast()
        || name.is_empty()
        || name.len() > 253
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b))
    {
        Err(invalid(
            "remote/endpoint",
            "advertised endpoint requires a unicast numeric address, nonzero port and TLS server name",
        ))
    } else {
        Ok(())
    }
}
pub(super) fn encode_hello(
    identity: &DatabaseIdentity,
    lease: &TransactorLease,
    instance: Digest,
) -> Result<Vec<u8>, SemanticError> {
    let mut bytes = HELLO.to_vec();
    bytes.extend_from_slice(&PROTOCOL.to_be_bytes());
    for name in [
        identity.database_id(),
        identity.lineage_id(),
        lease.holder_id.as_str(),
    ] {
        if name.len() > 64 * 1024 {
            return Err(invalid(
                "remote/identity-capacity",
                "remote identity metadata exceeds capacity",
            ));
        }
        bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
    }
    bytes.extend_from_slice(&lease.epoch.to_be_bytes());
    bytes.extend_from_slice(&instance);
    Ok(bytes)
}
pub(super) fn validate_hello(
    bytes: &[u8],
    endpoint: &RemoteWriterEndpoint,
) -> Result<(), SemanticError> {
    let expected = encode_hello(
        &endpoint.identity,
        &TransactorLease {
            database_id: endpoint.identity.database_id().into(),
            holder_id: endpoint.holder_id.clone(),
            epoch: endpoint.lease_epoch,
        },
        endpoint.instance_id,
    )?;
    if bytes != expected {
        Err(SemanticError::conflict(
            "remote/stale-endpoint",
            "authenticated server does not match the discovered database, writer lease or endpoint instance",
        ))
    } else {
        Ok(())
    }
}
