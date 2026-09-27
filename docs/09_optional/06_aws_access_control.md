# AWS Access Control

Deployment tools and supporting processes requiring access to services provided
by AWS, such as S3 or CloudWatch, must be authorized to do so. Use roles to grant
access to specific AWS resources through
[AWS Identity and Access Management](https://docs.aws.amazon.com/IAM/latest/UserGuide/best-practices.html).

EDB peers and transactors use PostgreSQL credentials for storage and a shared
token over verified TLS for remote transaction submission. The stock executable
does not call AWS APIs, obtain instance-role credentials, or integrate IAM with
database authorization. An IAM role does not replace these EDB credentials.
See [running on AWS](05_running_on_aws.md) for the deployment procedure.

## Using IAM Roles

Using IAM roles is the preferred method of granting AWS resource access to
deployment tools, log collectors and backup uploaders running in EC2. Roles
avoid distributing long-lived IAM user access keys in source code or
configuration files. Applications that use the role still receive temporary
credentials. An EC2 instance receives its role through an instance profile;
specify the role at launch or attach it to an existing instance. Each role can
be given policies granting access to specific AWS actions and resources. See
[IAM roles for Amazon EC2](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/iam-roles-for-amazon-ec2.html).

### Required Role Policies

There is no EDB runtime IAM policy to install. Select policies for the AWS
operations performed by your own infrastructure and supporting tools:

- A log collector needs the permissions required for its configured destination.
- A backup uploader needs access to the intended archive bucket and prefix.
  EDB itself writes a filesystem repository; uploading and downloading it are
  separate deployment operations. See [backup and restore](../08_operations/11_backup_and_restore.md).
- Deployment automation needs permissions to manage the resources it creates.
  Keep these privileges separate from those needed by runtime collectors.

When deployment automation passes an IAM role to EC2, it also needs permission
to pass that role. Restrict `iam:PassRole` to approved roles and the intended
service; the ability to attach a more powerful role can broaden what a deployer
can cause an instance to do. See [passing roles to AWS services](https://docs.aws.amazon.com/IAM/latest/UserGuide/id_roles_use_passrole.html).

## PostgreSQL Access

Create distinct administrative, writer and peer PostgreSQL roles. Use an
object-owning administrative account for installation and separate dedicated
runtime roles for transactors and peers. With those roles already created:

```sh
target/release/edb install --writer-role edb_writer --peer-role edb_peer
```

The command uses the administrative connection in `EDB_POSTGRES_URL`. It grants
both runtime roles database connection and schema usage privileges. Writers
receive `SELECT`, `INSERT` and `UPDATE` on `edb_objects`, and `SELECT`, `INSERT`,
`UPDATE` and `DELETE` on `edb_refs`. Peers receive `SELECT` on both tables.
Object deletion remains an administrative capability. The helper rejects
elevated roles and storage owners, but only adds grants: it neither revokes
existing permissions nor audits inherited access.

These are trusted storage credentials for the installation. They do not enforce
per-logical-database, entity or attribute authorization. Put application user
authorization in your application, and use separate installations when storage
clients must not trust one another. See [setting up storage services](../08_operations/06_storage_services.md)
and [storage permissions](../08_operations/00_deployment.md#provisioning-and-current-storage).

Supply writer credentials to the transactor and peer credentials to application
processes through their private PostgreSQL configuration. TCP connections use
verified TLS by default; `EDB_POSTGRES_TLS_ROOT` adds a PEM trust root when
needed. `sslmode=disable` explicitly selects plaintext. Keep connection strings
containing passwords out of source control and logs.

## Transaction and Network Access

The remote transaction listener requires a PEM certificate chain, a PKCS8 PEM
private key and a shared 256-bit token. Configure their paths with
`EDB_REMOTE_TLS_CERT`, `EDB_REMOTE_TLS_KEY` and `EDB_REMOTE_TOKEN_FILE`. The token
file contains 64 hexadecimal digits. Key and token files must be regular files
owned by the process user, with no group or other permissions, typically mode
`0600`. The credential loaders reject symlinks.

Clients need the same token, PostgreSQL peer credentials for reads and writer
discovery, and trust for the listener's certificate. `EDB_REMOTE_TLS_ROOT` adds
a PEM trust root; certificate and server-name verification remain mandatory.
Possession of the token grants transaction submission to the listener's
database. It does not identify individual application users or grant storage
reads. Each listener accepts one token; replacing the file does not update a
running process. Coordinate replacement and restarts with clients. See
[remote credentials and TLS](04_remote_transactions.md#credentials-and-tls).

Restrict network access to PostgreSQL and the advertised transaction address
to the hosts that need it. Keep the optional health listener on loopback or a
restricted monitoring network: `/health` and `/ready` are plaintext status
endpoints without token authentication. TLS and a shared token do not replace
network controls. See [AWS network configuration](05_running_on_aws.md#prepare-the-network-and-instance-configuration).

## Using IAM User Access Keys

AWS recommends temporary credentials for workloads through IAM roles, and
federated temporary credentials for human users. Initial provisioning does not
require a blanket exception for long-lived user keys. See
[IAM security best practices](https://docs.aws.amazon.com/IAM/latest/UserGuide/best-practices.html).

`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` are not EDB configuration.
Setting them does not authorize PostgreSQL access or transaction submission.
EDB has no AWS credential parameters in storage or backup URIs, and its native
process does not consume Java AWS properties. Any AWS credential handling
belongs to the separate tools that call AWS services.

## Migrating from User Access Keys to IAM Roles

Identify the deployment tools, collectors and uploaders that currently use
long-lived AWS credentials. Give each workload an appropriate role, configure
its credential provider to use that role, and verify the required operations.
Then remove the obsolete key configuration and retire the unused keys through
your AWS administration procedure. Follow the
[IAM guidance on temporary credentials and unused access](https://docs.aws.amazon.com/IAM/latest/UserGuide/best-practices.html).

This migration changes AWS access for those tools. Continue to provision and
protect PostgreSQL credentials, TLS private keys and transaction tokens
separately.
