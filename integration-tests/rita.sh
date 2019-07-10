#!/usr/bin/env bash
# export PATH="$PATH:$HOME/.cargo/bin"
BABELD_DIR="deps/babeld"
BOUNTY_HUNTER_DIR="/tmp/bounty_hunter"
NETLAB_PATH="deps/network-lab/network-lab.sh"

REMOTE_A=${REMOTE_A:=https://github.com/althea-mesh/althea_rs.git}
REVISION_A=${REVISION_A:=""}
DIR_A=${DIR_A:=althea_rs_a} # Don't override without good reason, this one and $DIR_B are git ignored
TARGET_DIR_A=${TARGET_DIR_A:=target_a} # Don't override without good reason, this one and $TARGET_DIR_B are git ignored

REMOTE_B=${REMOTE_B:=$REMOTE_A}
REVISION_B=${REVISION_B:=$RELEASE_A}
DIR_B=${DIR_B:=althea_rs_b}
TARGET_DIR_B=${TARGET_DIR_B:=target_b}

set -euxo pipefail

cd $(dirname $0) # Make the script runnable from anywhere

# Loads module if not loaded and available, does nothing if already loaded and fails if not available
set +e
sudo modprobe wireguard
set -e
set -e
# sets up bounty hunter cers
openssl req -newkey rsa:2048 -nodes -keyform pem -keyout bh_key.pem -x509 -days 365 -outform pem -out bh_cert.pem -subj "/C=US/ST=Althea/L=Althea/O=Althea/OU=Althea/CN=Althea"
export BOUNTY_HUNTER_CERT=$PWD/bh_cert.pem
export BOUNTY_HUNTER_KEY=$PWD/bh_key.pem
set +e
cargo install diesel_cli
sudo cp $(which diesel) /usr/bin
set -e
# we need to start the database again in the namespace, so we have to kill it out here
# this sends sigint which should gracefully shut it down but terminate existing connections
set +e
sudo killall -2 postgres
set -e

build_rev() {
  remote=$1
  revision=$2
  dir=$3
  target_dir=$4

  if [ -z "${VERBOSE-}" ] ; then
    git --no-pager show
  fi

  mkdir -p $target_dir

  if [ -z "${NO_PULL-}" ] ; then
    rm -rf $dir
    git clone $remote $dir
  fi

  pushd $dir
    git checkout $revision
    cargo build --all
  popd
}

pip3 install --user -r requirements.txt

if [ ! -f "${NETLAB_PATH-}" ] ; then
  git clone "https://github.com/kingoflolz/network-lab" "deps/network-lab" # TODO: Change this back when PR is upstreamed
fi

chmod +x deps/network-lab deps/network-lab/network-lab.sh

# Build Babel if not built
if [ ! -f "${BABELD_DIR-}/babeld" ]; then
  rm -rf $BABELD_DIR
  git clone -b master https://github.com/althea-mesh/babeld.git $BABELD_DIR
  make -C $BABELD_DIR
fi

# Build BH if not built
if [ ! -d "${BOUNTY_HUNTER_DIR}" ]; then
  rm -rf BOUNTY_HUNTER_DIR
  git clone -b master https://github.com/althea-net/bounty_hunter.git $BOUNTY_HUNTER_DIR
  pushd $BOUNTY_HUNTER_DIR
     cargo build
  popd
fi


# Only care about revisions if a compat layout was picked
if [ ! -z "${COMPAT_LAYOUT-}" ] ; then
  build_rev $REMOTE_A "$REVISION_A" $DIR_A $TARGET_DIR_A
  export RITA_A="$target_dir/debug/rita"
  export RITA_EXIT_A="$target_dir/debug/rita_exit"
  export BOUNTY_HUNTER_A="$BOUNTY_HUNTER_DIR/target/debug/bounty_hunter"
  export DIR_A=$DIR_A
  cp -r $DIR_A/target/* $target_dir
 
  build_rev $REMOTE_B "$REVISION_B" $DIR_B $TARGET_DIR_B
  export RITA_B="$target_dir/debug/rita"
  export RITA_EXIT_B="$target_dir/debug/rita_exit"
  export BOUNTY_HUNTER_B="$BOUNTY_HUNTER_DIR/target/debug/bounty_hunter"
  export DIR_B=$DIR_B
  cp -r $DIR_B/target/* $target_dir
else
  pushd ..
    cargo build --all
  popd
fi


sudo -E PATH="$PATH:$HOME/.cargo/bin" python3 integration-test-script/rita.py $@
