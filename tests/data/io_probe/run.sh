#!/bin/bash
# Builds the probe with Lean 4.34.0 and rewrites the outputs `tests/io_rows.rs` reads.
# From leanrs's rt/leanrs_rt/tests/data/io_probe/run.sh (the same scenarios).
set -e
cd "$(dirname "$0")"
T=${LEAN_TOOLCHAIN:-$HOME/.elan/toolchains/leanprover--lean4---v4.34.0}/bin
P=/tmp/lean-runtime-io-probe-bin
$T/lean Main.lean -c $P.c
$T/leanc -O2 -o $P $P.c shim.c
$P errno > errno.txt
D=/tmp/lean-runtime-io-probe
rm -rf $D && mkdir -p $D/names
ln -s full/f $D/link
ln -s nowhere $D/dangling
touch $D/names/$'a\xffb' $D/names/$'\xc0\x80'
printf 'ok\n\xc0\x80x\n\xe2\x82' > $D/bytes.txt
(cd $D && $P fs $D < /dev/null) > fs.txt
# Block policy: bytes visible after `IO.Process.forceExit` (stdout to a pipe, to a file; a file handle).
: > buf.txt
for ops in "w3000" "w4096" "w5000" "w10000" "w8192" "w8193" "w100 f w4096" "w100 f w4097" \
           "w100 f w8192" "w100 w4000" "w100 w3996" "w100 w3997" "w4095 w1" "w4095 w2" "w1 w8191" \
           "w1 w8192" "w1 w12288"; do
  pipe=$($P buf stdout $ops | wc -c)
  $P buf stdout $ops > $D/out.txt; file=$(stat -c %s $D/out.txt)
  rm -f $D/h.txt; $P buf $D/h.txt $ops; handle=$(stat -c %s $D/h.txt)
  echo "$ops: pipe=$pipe file=$file handle=$handle" >> buf.txt
done
# A readWrite handle over 20000 bytes `a`: the `x` bytes on disk after `forceExit`.
: > bufrw.txt
for ops in "r1 w100" "r1 w4096" "r1 w5000" "R w100" "R w4096" "w100 R w4096" "w10 r1 w4096" \
           "w100 f w4096" "r1 f w4096"; do
  python3 -c "open('$D/rw.txt','wb').write(b'a'*20000)"
  $P bufrw $D/rw.txt $ops
  echo "$ops: x=$(tr -cd x < $D/rw.txt | wc -c)" >> bufrw.txt
done
