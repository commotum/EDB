# Running on AWS

This document explains how to launch your transactor on AWS. Before you read
this, read through [Setting up Storage Services](../08_operations/06_storage_services.md),
configure PostgreSQL, and test it with a transactor running locally. When you
know your transactor and storage service are working, follow the steps below
to launch your transactor on AWS.

This is deployment guidance for infrastructure you operate. EDB supplies the
native `edb` executable and PostgreSQL storage implementation; it does not
supply an AWS AMI, CloudFormation template, provisioning commands, or a tested
AWS service compatibility matrix. Validate your selected operating system,
PostgreSQL service, credentials, networking and recovery procedures before
using the deployment for production work.

Commands using `target/release/edb` below run from the EDB checkout root. If you
install the executable elsewhere, substitute its installed path.

## Starting the Transactor on AWS

You can deploy a transactor on an EC2 instance using your own image and
infrastructure automation. Choose an instance size from measurements of your
workload; see [capacity planning](../08_operations/09_capacity_planning.md).
Build for the target machine's architecture and operating system, and include
the native TLS runtime dependencies required by that build.

A deployment needs PostgreSQL storage, a transactor, and application processes
that read through the native peer library. Both the transactor and applications
connect to PostgreSQL. Applications submit transactions separately to the
transactor's verified TLS endpoint. S3 and DynamoDB are not EDB storage
providers.

### Prepare Storage and Credentials

1. Provision PostgreSQL and a dedicated empty schema. Select the same database
   and schema for all EDB processes. A hosted PostgreSQL service must permit
   the storage installation, runtime grants and SQL operations EDB needs;
   verify these with the chosen service and configuration.
2. Create distinct PostgreSQL administrative, writer and peer LOGIN roles.
   Use an object-owning administrative account for installation. Runtime roles
   must not own the storage schema/tables or have elevated administrative
   privileges.
3. Configure `EDB_POSTGRES_URL` for the administrative connection, including
   the intended schema through `search_path`. TCP uses verified TLS by default.
   Set `EDB_POSTGRES_TLS_ROOT` if an additional PEM trust root is needed.
   Supply passwords through your private deployment configuration, keeping
   connection strings out of source control and logs.
4. Build, install the storage tables, and create the logical database:

   ```sh
   cargo build --locked --release --bin edb
   target/release/edb install --writer-role edb_writer --peer-role edb_peer
   target/release/edb create --database orders
   ```

5. Configure the transactor with writer PostgreSQL credentials and applications
   with peer credentials. Runtime startup validates existing storage; it does
   not install tables or create a database.

PostgreSQL roles and AWS IAM roles serve different purposes. The storage roles
are trusted access to the EDB installation, not per-logical-database SQL
authorization. The grant helper adds permissions and does not revoke prior
grants or inherited access. See [storage permissions](../08_operations/00_deployment.md#provisioning-and-current-storage).

For deployment tools or log collectors that call AWS APIs from EC2, use an
instance IAM role with permissions limited to the required actions and
resources. EC2 exposes the role through an instance profile and provides
temporary credentials to the application. An instance role does not configure
EDB's PostgreSQL credentials or remote transaction token. See
[IAM roles for Amazon EC2](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/iam-roles-for-amazon-ec2.html).

### Prepare the Network and Instance Configuration

Keep a versioned infrastructure definition that specifies the image or binary
version, instance size, network placement, security groups, process supervisor,
and paths for runtime configuration. Store secret values separately.

Configure private connectivity for these flows, adjusting ports to your
installation:

| Connection | Destination | Access |
| --- | --- | --- |
| Transactor and peers to PostgreSQL | PostgreSQL's configured TCP port | Only the intended writer, application and administrative hosts |
| Applications to the transactor | The advertised transaction port, for example TCP 7443 | Authorized application hosts |
| Monitoring to the health listener | The separately configured health port | Local monitoring or a restricted monitoring network |
| Deployment tools and collectors to AWS services | The endpoints used by your automation | Only the access required by that automation |

Security group rules can select protocols, ports, CIDR ranges and security
group sources. Merely placing two instances in the same security group does
not establish the required inbound rule. Check outbound rules and network
routing as well. See [AWS security group rules](https://docs.aws.amazon.com/vpc/latest/userguide/security-group-rules.html).

Do not use unrestricted public ingress as the default. If applications run
outside the VPC, provide a deliberate route to both PostgreSQL and the
advertised transaction address, and restrict access to those clients.
Transaction TLS and token authentication do not replace network access control.

Provision the transactor's certificate chain, PKCS8 PEM private key, and shared
256-bit transaction token. The token file contains 64 hexadecimal digits.
Private key and token files must be regular files owned by the process user,
with no group or other permissions; the loaders reject symlinks. Configure
clients with the same token and trust for the server certificate. See
[remote transactions](04_remote_transactions.md#credentials-and-tls).

### Launch the Transactor

With writer PostgreSQL configuration already supplied to the process:

```sh
export EDB_REMOTE_TLS_CERT=/private/edb/server-chain.pem
export EDB_REMOTE_TLS_KEY=/private/edb/server-key.pem
export EDB_REMOTE_TOKEN_FILE=/private/edb/remote-token

target/release/edb transactor \
  --database orders \
  --listen 0.0.0.0:7443 \
  --advertise 10.0.1.10:7443 \
  --tls-server-name writer-a.example.com \
  --mode auto \
  --health-listen 127.0.0.1:8080 \
  --telemetry-ms 10000
```

Replace the advertised IP with this instance's reachable numeric address and
the server name with the name on its certificate. `--listen` is a local bind
address; `--advertise` is the address clients actually use. The wildcard bind
address does not itself make the instance publicly reachable. Server-name
verification is independent of the numeric routing address.

Run this foreground process under your supervisor. `--mode auto` waits for
writer authority if another transactor is active. The process publishes its
endpoint in PostgreSQL after activation and prints `READY` when the listener
is available. `STANDBY` means it is waiting, not ready to accept writes. The
default `--mode active` fails on contention.

For a CloudFormation deployment, put these configuration and launch steps in
your own template and bootstrap procedure, then create the stack with your
normal AWS tooling. Monitor stack events and process output independently:
an infrastructure resource reaching its expected state is not proof that EDB
has reached `READY` or that application transactions succeed.

### Connecting to the Transactor on AWS

Applications need PostgreSQL peer credentials, the shared transaction token,
and, when necessary, `EDB_REMOTE_TLS_ROOT` for the remote endpoint's trust root.
They discover the active writer through storage and connect to its advertised
address. Discovery authenticates the database identity, fenced writer lease
and listener instance. See [connecting to the remote endpoint](04_remote_transactions.md#connecting-to-the-remote-endpoint).

Test a read and transaction from an actual application host after launch. A
successful connection from the transactor host alone does not establish the
application's network path or permissions. Do not distribute writes through
a load balancer that selects arbitrary listeners: the listener must match the
instance identified by authenticated discovery.

## Using the AWS Management Console

Once you are up and running, you may want to make changes to your configuration.
Use the consoles for the infrastructure you provisioned:

- In CloudFormation, inspect stack events and the resources in your template.
- In EC2, inspect instance state and security group rules for the configured
  PostgreSQL, transaction and monitoring ports.
- In your PostgreSQL service or monitoring system, inspect storage capacity,
  connection pressure, I/O and backup status.
- In CloudWatch, inspect the host metrics and log streams you have configured.
- In S3, inspect log archives or backup copies only if your own tooling uploads
  them there.

Record operational changes in the infrastructure definition so a replacement
instance gets the intended configuration. Before deleting a stack, identify
which storage and backup resources must survive. CloudFormation's
`DeletionPolicy` can retain resources or, for resource types that support it,
take snapshots on deletion; configure the policy explicitly for your resources.
See [DeletionPolicy](https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/aws-attribute-deletionpolicy.html).
Stopping a transactor and deleting authoritative storage are separate actions.

## Using S3 Log Storage and CloudWatch Metrics

The stock transactor writes lifecycle output to stdout and errors to stderr.
The opt-in `--telemetry-ms` setting emits bounded JSON operational events to
stdout. EDB does not create log buckets, copy logs to S3, or publish directly
to CloudWatch. There are no AWS log or metrics properties to enable in the
transactor. See [logging](../08_operations/13_logging.md) and
[observability](../08_operations/03_observability.md).

To collect logs, configure your supervisor to capture process output, and
configure a collector for the destination you use. The AWS CloudWatch agent
can collect EC2 host metrics and logs; installing it, choosing files and log
groups, and granting its IAM permissions are deployment tasks. EDB's JSON
events do not automatically become CloudWatch metrics. Configure any metric
extraction explicitly. See [the CloudWatch agent guide](https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/Install-CloudWatch-Agent.html).

For S3 archives, arrange a separate uploader with permissions restricted to the
intended bucket and prefix. Set archive retention and encryption through your
storage policy. Keep credentials, plaintext request keys and application data
out of operational logs; infrastructure collection does not make sensitive
output safe to publish.

## High Availability and Backups

Run two transactors on separate machines with `--mode auto` and the same
PostgreSQL namespace and logical database. Give each its own advertised
address and appropriate certificate. Clients need trust for both certificates
and the shared token. The first process to claim the guarded writer lease
becomes active; the other waits. Supervise both processes. See
[high availability](../08_operations/10_high_availability.md).

Choose placement that separates the machine failures you intend to tolerate.
Transactor takeover still requires available PostgreSQL storage. Configure
PostgreSQL replication, failover and physical backups independently; EDB's
writer lease does not provide these facilities. Measure takeover and client
recovery under the network and storage failures relevant to your deployment.
Lease settings alone do not promise a recovery-time bound.

Maintain EDB portable backups in addition to your PostgreSQL recovery policy.
For example, with a private filesystem repository:

```sh
target/release/edb backup --database orders --repository /private/backups/orders
target/release/edb list-backups --repository /private/backups/orders
```

Use the returned basis and generation to verify the selected point with
`edb verify-backup`. The repository is a filesystem directory, not an S3 URI.
If you archive it to S3, your tooling must preserve a complete repository and
recover it to a local directory for verification and restore. EDB does not
encrypt repositories. Storage retention must cover backup-copy duration, and
a completed copy is not itself deep semantic verification. Rehearse recovery
to a separately installed target using [backup and restore](../08_operations/11_backup_and_restore.md).

## Troubleshooting the Transactor Instance

Treat the transactor machine as replaceable compute. Keep authoritative state
in PostgreSQL, and preserve logs and backup repositories independently of the
instance. The optional local SSD cache is disposable; it is not a backup.

If a transactor fails, inspect its exit status and logs before replacement.
Check PostgreSQL connectivity and grants, schema selection, TLS trust and
certificate names, credential-file ownership, advertised-address reachability,
and writer contention. The optional `/health` and `/ready` endpoints report
process phase without database I/O; they are plaintext status endpoints, not
storage integrity checks. Keep them inside the monitoring boundary.

An active stock process exits unsuccessfully when service or listener
availability is lost; its supervisor can restart it. Replacing a process does
not repair damaged storage. Current-index damage requires explicit recovery;
canonical or retained-history damage requires separate investigation and
recovery. See [index recovery](../08_operations/00_deployment.md#explicit-index-recovery).

After a lost transaction response, reconcile or retry with the same request
key and identical content. A timeout is not proof of rollback. Confirm reads,
writes and exact retries after recovery before returning the system to normal
operation.

## Understanding Provisioning Commands

EDB does not have AWS "ensure" commands that create infrastructure and emit
completed property files. Keep infrastructure provisioning in your deployment
automation and EDB installation in an explicit administrative step.

Repeating `edb install` on a matching current store is idempotent; it does not
create the PostgreSQL database/schema or upgrade an incompatible storage
format. Installation and runtime grants commit separately, so an `INSTALLED`
line can precede a grant failure. Correct the roles and repeat installation.
`edb create --database orders` is idempotent for an existing active catalog
name. Neither command makes the surrounding AWS deployment ready or validates
application connectivity. See [administration](../08_operations/01_administration.md).
