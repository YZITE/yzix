make ARCH=x86 headers
mkdir -p $out
cp -r usr/include $out
find $out -type f ! -name '*.h' -delete
