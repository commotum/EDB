use super::*;

impl PortableBackup {
    pub fn connect(connection: &str) -> Result<Self, SemanticError> {
        Self::connect_configured(&PostgresConnectionConfig::parse(connection)?)
    }
    pub fn connect_configured(
        connection: &PostgresConnectionConfig,
    ) -> Result<Self, SemanticError> {
        drop(PgBlockStore::connect(connection)?);
        Ok(Self {
            connection: connection.clone(),
            maintenance: MaintenanceControl::default(),
        })
    }
    pub fn with_maintenance_control(mut self, control: MaintenanceControl) -> Self {
        self.maintenance = control;
        self
    }
    pub fn backup_database(
        &mut self,
        name: &str,
        directory: &Path,
    ) -> Result<BackupPoint, SemanticError> {
        self.backup_database_with_fault(name, directory, BackupFault::None)
    }
    #[doc(hidden)]
    pub fn backup_database_with_fault(
        &mut self,
        name: &str,
        directory: &Path,
        fault: BackupFault,
    ) -> Result<BackupPoint, SemanticError> {
        self.capture(name, directory, fault, || {})
    }
    #[doc(hidden)]
    pub fn backup_database_with_capture_probe<F: FnOnce()>(
        &mut self,
        name: &str,
        directory: &Path,
        probe: F,
    ) -> Result<BackupPoint, SemanticError> {
        self.capture(name, directory, BackupFault::None, probe)
    }
    fn capture<F: FnOnce()>(
        &mut self,
        name: &str,
        directory: &Path,
        fault_at: BackupFault,
        probe: F,
    ) -> Result<BackupPoint, SemanticError> {
        self.maintenance.check()?;
        let directory = repository::anchor(directory)?;
        repository::admit(&directory, true)?;
        let database = BlockDatabase::resolve(&self.connection, name)?;
        let reader = BlockReader::connect(&self.connection, BlockReadConfig::default())?;
        let capture = reader.capture_reference(&database.reference_key())?;
        probe();
        inject(
            fault_at,
            BackupFault::AfterPublicationCaptured,
            "backup/after-publication-captured",
        )?;
        let mut source = PgBlockStore::connect(&self.connection)?;
        let publication_id = capture.root_id();
        let publication =
            DatabaseRoot::decode(&publication_id, &source.read_object(publication_id)?)?;
        let metadata_id = publication
            .metadata
            .ok_or_else(|| fault("backup/metadata", "Publication lacks metadata"))?;
        let metadata = SnapshotMetadata::decode(&metadata_id, &source.read_object(metadata_id)?)?;
        if metadata.identity != publication.identity || metadata.basis != publication.basis {
            return Err(fault(
                "backup/coordinate",
                "Publication and metadata disagree",
            ));
        }
        repository::claim(&directory, publication.identity)?;
        if repository::point_path(&directory, metadata.generation, publication.basis)
            .try_exists()
            .map_err(repository::io)?
        {
            let prior = load_selected(&directory, Some(metadata.generation), publication.basis)?;
            same_information(&publication, &load_publication(&directory, &prior)?)?;
            let reused = walk_repository(
                &directory,
                &[prior.point.manifest_hash],
                &self.maintenance,
                false,
            )?;
            return Ok(BackupPoint {
                objects_reused: reused,
                ..prior.point
            });
        }
        let mut copy = RepositoryCopy {
            directory: directory.clone(),
            source,
            written: 0,
            reused: 0,
        };
        let mut seen = BTreeSet::new();
        let mut pending = vec![publication_id];
        while let Some(id) = pending.pop() {
            self.maintenance.check()?;
            if !seen.insert(id) {
                continue;
            }
            let bytes = copy.read_object(id)?;
            pending.extend(crate::storage::ownership::object_children(
                &mut copy, id, &bytes,
            )?);
            copy.put_object(&bytes)?;
            if seen.len() == 1 {
                inject(
                    fault_at,
                    BackupFault::AfterFirstObjectStaged,
                    "backup/after-first-object-staged",
                )?;
            }
            if seen.len() % 128 == 0 {
                self.maintenance.after_batch()?;
            }
        }
        self.maintenance.after_batch()?;
        inject(fault_at, BackupFault::AfterObjects, "backup/after-objects")?;
        let mut payload = publication.identity.to_vec();
        payload.extend_from_slice(&metadata.generation.to_be_bytes());
        payload.extend_from_slice(&publication.basis.to_be_bytes());
        let manifest = Block {
            kind: POINT_KIND,
            links: vec![publication_id],
            payload,
        }
        .encode()?;
        let manifest_hash = copy.put_object(&manifest)?;
        self.maintenance.after_batch()?;
        inject(
            fault_at,
            BackupFault::AfterManifestStaged,
            "backup/after-manifest-staged",
        )?;
        let point = if repository::publish_point(
            &directory,
            metadata.generation,
            publication.basis,
            &manifest,
        )? {
            BackupPoint {
                lineage_id: crate::storage::catalog::identity_string(publication.identity),
                log_generation: metadata.generation,
                basis_t: publication.basis,
                manifest_hash,
                objects_written: copy.written,
                objects_reused: copy.reused,
            }
        } else {
            let prior = load_selected(&directory, Some(metadata.generation), publication.basis)?;
            same_information(&publication, &load_publication(&directory, &prior)?)?;
            BackupPoint {
                objects_written: copy.written,
                objects_reused: copy.reused,
                ..prior.point
            }
        };
        inject(
            fault_at,
            BackupFault::AfterManifestPublished,
            "backup/after-manifest-published",
        )?;
        Ok(point)
    }
    pub fn list_backup_points(directory: &Path) -> Result<Vec<BackupPoint>, SemanticError> {
        let directory = repository::anchor(directory)?;
        repository::admit(&directory, false)?;
        let mut result = Vec::new();
        for entry in std::fs::read_dir(directory.join("points")).map_err(repository::io)? {
            let entry = entry.map_err(repository::io)?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| fault("backup/point-name", "Point name is not UTF8"))?;
            if name.starts_with(".atomic-stage-") {
                continue;
            }
            let (generation, basis) = parse_point_name(name)?;
            result.push(point_header(&directory, generation, basis)?.0);
        }
        result.sort_by_key(|point| (point.basis_t, point.log_generation));
        Ok(result)
    }
    pub fn list_backups(directory: &Path) -> Result<Vec<u64>, SemanticError> {
        let mut result: Vec<_> = Self::list_backup_points(directory)?
            .into_iter()
            .map(|p| p.basis_t)
            .collect();
        result.dedup();
        Ok(result)
    }
    pub fn verify_backup_presence(
        directory: &Path,
        basis: u64,
    ) -> Result<BackupPoint, SemanticError> {
        let point = load_selected(directory, None, basis)?;
        walk_repository(
            directory,
            &[point.point.manifest_hash],
            &MaintenanceControl::default(),
            false,
        )?;
        Ok(point.point)
    }
    pub fn verify_backup_point_presence(
        directory: &Path,
        basis: u64,
        generation: u64,
    ) -> Result<BackupPoint, SemanticError> {
        let point = load_selected(directory, Some(generation), basis)?;
        walk_repository(
            directory,
            &[point.point.manifest_hash],
            &MaintenanceControl::default(),
            false,
        )?;
        Ok(point.point)
    }
    pub fn verify_backup(
        directory: &Path,
        basis: u64,
        deep: bool,
    ) -> Result<BackupVerification, SemanticError> {
        verify_read_point(
            directory,
            load_selected(directory, None, basis)?,
            deep,
            &MaintenanceControl::default(),
        )
    }
    pub fn verify_backup_point(
        directory: &Path,
        basis: u64,
        generation: u64,
        deep: bool,
    ) -> Result<BackupVerification, SemanticError> {
        verify_read_point(
            directory,
            load_selected(directory, Some(generation), basis)?,
            deep,
            &MaintenanceControl::default(),
        )
    }
}

struct RepositoryCopy {
    directory: PathBuf,
    source: PgBlockStore,
    written: usize,
    reused: usize,
}
impl ObjectReader for RepositoryCopy {
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        if repository::contains(&self.directory, id)? {
            read_object(&self.directory, id)
        } else {
            self.source.read_object(id)
        }
    }
}
impl ObjectWriter for RepositoryCopy {
    fn put_object(&mut self, bytes: &[u8]) -> Result<ObjectId, SemanticError> {
        let (id, written) = repository::put_object(&self.directory, bytes)?;
        if written {
            self.written += 1;
        } else {
            self.reused += 1;
        }
        Ok(id)
    }
    fn flush_objects(&mut self) -> Result<(), SemanticError> {
        // Repository puts sync new files/directories and authenticate reuse.
        Ok(())
    }
}
fn inject(
    actual: BackupFault,
    expected: BackupFault,
    code: &'static str,
) -> Result<(), SemanticError> {
    if actual != expected {
        return Ok(());
    }
    let category = if actual == BackupFault::AfterManifestPublished {
        crate::ErrorCategory::UnknownOutcome
    } else {
        crate::ErrorCategory::Fault
    };
    Err(SemanticError::new(
        category,
        code,
        "Injected backup interruption",
    ))
}
