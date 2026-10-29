#!/bin/sh
# All services and storage are inside this disposable container; no host ports.
set -eu
. /usr/local/bin/acceptance-wait-ready
umask 077
# One installed executable must not broaden the delegated launcher boundary.
assert_launcher_denied() {
  if sudo -u enumeray sudo -n -u model -- /usr/local/bin/enumeray "$@" >/dev/null 2>&1; then
    printf '%s\n' 'Restricted launcher permitted a non-runner command' >&2
    exit 1
  fi
}
assert_launcher_denied auth whoami
assert_launcher_denied connector run --help
sudo -u enumeray sudo -n -u model -- /usr/local/bin/enumeray compute-runner --help >/dev/null
mkdir -p /run/sshd /run/munge /run/mysqld
chown munge:munge /run/munge
chmod 0755 /run/munge /run/mysqld
install -d -m 0755 -o slurm -g slurm /run/slurm
chown mysql:mysql /run/mysqld
if [ ! -e /etc/munge/munge.key ]; then dd if=/dev/urandom of=/etc/munge/munge.key bs=1024 count=1 2>/dev/null; fi
chown munge:munge /etc/munge/munge.key
chmod 0400 /etc/munge/munge.key
sudo -u munge /usr/sbin/munged --foreground &
/usr/sbin/mariadbd --user=mysql --skip-networking &
wait_ready 180 MariaDB mariadb-admin ping --silent
mariadb -e "CREATE DATABASE IF NOT EXISTS slurm_acct_db; CREATE USER IF NOT EXISTS 'slurm'@'localhost' IDENTIFIED BY 'isolated-acceptance'; GRANT ALL ON slurm_acct_db.* TO 'slurm'@'localhost';"
cat > /etc/slurm/slurmdbd.conf <<'CONFIG'
AuthType=auth/munge
DbdHost=localhost
DbdPort=6819
SlurmUser=slurm
LogFile=/var/log/slurm/slurmdbd.log
PidFile=/run/slurm/slurmdbd.pid
StorageType=accounting_storage/mysql
StorageHost=localhost
StorageLoc=slurm_acct_db
StorageUser=slurm
StoragePass=isolated-acceptance
CONFIG
chown slurm:slurm /etc/slurm/slurmdbd.conf
chmod 0600 /etc/slurm/slurmdbd.conf
/usr/sbin/slurmdbd -D &
wait_ready 180 'Slurm accounting' sacctmgr -n show cluster
sacctmgr -i add cluster enumeray-acceptance
sacctmgr -i add account research Description=Acceptance Organization=Enumeray
sacctmgr -i add user model Account=research DefaultAccount=research
# No cgroup controllers are needed with task/none and linuxproc tracking.
if [ "${ENUMERAY_BACKEND_CPUS:-1}" = 2 ]; then
  # Optional consumers can reserve both Slurm execution slots concurrently.
  sed -i 's/CPUs=1 RealMemory=512/CPUs=2 RealMemory=1024/' /etc/slurm/slurm.conf
fi
printf 'CgroupPlugin=cgroup/v1\n' > /etc/slurm/cgroup.conf
chmod 0644 /etc/slurm/cgroup.conf
/usr/sbin/slurmctld -D &
/usr/sbin/slurmd -D &
ssh-keygen -A >/dev/null
install -d -m 0700 -o enumeray -g enumeray /home/enumeray/.ssh
install -d -m 0700 -o model -g model /home/model/.ssh
sudo -u enumeray ssh-keygen -q -t ed25519 -N '' -f /home/enumeray/.ssh/id_ed25519
cp /home/enumeray/.ssh/id_ed25519.pub /home/model/.ssh/authorized_keys
chown model:model /home/model/.ssh/authorized_keys
chmod 0600 /home/model/.ssh/authorized_keys
/usr/sbin/sshd -p 2222 -o PasswordAuthentication=no -o PermitRootLogin=no
ssh-keyscan -p 2222 127.0.0.1 > /home/enumeray/.ssh/known_hosts 2>/dev/null
chown enumeray:enumeray /home/enumeray/.ssh/known_hosts
chmod 0600 /home/enumeray/.ssh/known_hosts
slurm_node_ready() { sinfo -h -o '%T' 2>/dev/null | grep -q '^idle'; }
wait_ready 180 'an idle Slurm node' slurm_node_ready
sinfo
# Consumers may supply their own scenario after the driver environment is ready.
if [ "$#" -gt 0 ]; then exec "$@"; fi
sudo -u enumeray env ENUMERAY_REAL_BACKENDS=1 /usr/local/bin/driver-tests compute::tests::real_ssh_and_slurm --ignored --exact --nocapture --test-threads=1
