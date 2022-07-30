set -xe
cd ..
mkdir build
cd build
"../$sourceRoot/configure" --prefix="$out"
make
make install
