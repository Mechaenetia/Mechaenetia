#!/bin/sh

echo "Formatting all Files"

SOURCE_DIR=$(dirname "$0")

find "$SOURCE_DIR/../assets" -iname "*.png" -mtime -30 -exec optipng -o7 -clobber -silent {} \;

echo "Done formatting all Files"
