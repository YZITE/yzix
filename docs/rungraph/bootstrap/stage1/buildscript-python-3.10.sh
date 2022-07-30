set -xe
NIX_WRAPPER_gcc_ARGS="$NIX_WRAPPER_gcc_ARGS -lgcc_s"
NIX_WRAPPER_gxx_ARGS="$NIX_WRAPPER_gxx_ARGS -lgcc_s"
export NIX_WRAPPER_gcc_ARGS NIX_WRAPPER_gxx_ARGS
ls -las
./configure --prefix="$out" \
    --enable-shared \
    --without-ensurepip \
    ac_cv_func_lchmod=no \

make -j4
make install
# needed for some packages, especially packages that backport functionality
# to 2.x from 3.x
for item in $out/lib/${libPrefix}/test/*; do
  if [[ "$item" != */test_support.py*
     && "$item" != */test/support
     && "$item" != */test/libregrtest
     && "$item" != */test/regrtest.py* ]]; then
    rm -rf "$item"
  else
    echo $item
  fi
done
touch $out/lib/${libPrefix}/test/__init__.py

ln -s ${libPrefix}m $out/include/${libPrefix}

# Determinism: Windows installers were not deterministic.
# We're also not interested in building Windows installers.
find "$out" -name 'wininst*.exe' | xargs -r rm -f

# Use Python3 as default python
ln -s idle3 "$out/bin/idle"
ln -s pydoc3 "$out/bin/pydoc"
ln -s python3 "$out/bin/python"
ln -s python3-config "$out/bin/python-config"
ln -s python3.pc "$out/lib/pkgconfig/python.pc"
# Get rid of retained dependencies on -dev packages, and remove
# some $TMPDIR references to improve binary reproducibility.
# Note that the .pyc file of _sysconfigdata.py should be regenerated!
for i in $out/lib/${libPrefix}/_sysconfigdata*.py $out/lib/${libPrefix}/config-3.10*/Makefile; do
   sed -i $i -e "s|/tmp|/no-such-path|g"
done

strip -S $out/lib/${libPrefix}/config-*/libpython*.a || true
rm -fR $out/bin/python*-config $out/lib/python*/config-*
rm -fR $out/bin/idle* $out/lib/python*/{idlelib,turtledemo}
rm -fR $out/lib/python*/tkinter
rm -fR $out/lib/python*/test $out/lib/python*/**/test{,s}
find $out -type d -name __pycache__ -print0 | xargs -0 -I {} rm -rf "{}"
mkdir -p $out/share/gdb
sed '/^#!/d' Tools/gdb/libpython.py > $out/share/gdb/libpython.py
