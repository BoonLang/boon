#!/bin/bash
# Escapes input for safe shell argument passing
# Uses printf %q which properly escapes all special characters

printf '%q' "$*"
