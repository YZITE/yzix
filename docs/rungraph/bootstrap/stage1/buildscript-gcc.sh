set -xe
sed -e '/m64=/s/lib64/lib/' -i.orig gcc/config/i386/t-linux64
cd ..
mkdir build
cd build
"../$sourceRoot/configure" --prefix="$out" \
  --disable-libcc1 \
  --disable-bootstrap \
  --with-newlib \
  --without-headers \
  --enable-initfini-array \
  --disable-nls \
  --disable-shared \
  --disable-multilib \
  --disable-decimal-float \
  --disable-threads \
  --disable-libatomic \
  --disable-libgomp \
  --disable-libquadmath \
  --disable-libssp \
  --disable-libvtv \
  --disable-libstdcxx \
  --enable-languages=c,c++ \

make -j4
make install

cat ../$sourceRoot/gcc/limitx.h ../$sourceRoot/gcc/glimits.h ../$sourceRoot/gcc/limity.h > \
  `dirname $($out/bin/x86_64-pc-linux-gnu-gcc -print-libgcc-file-name)`/install-tools/include/limits.h
