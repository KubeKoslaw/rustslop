# Rusted rice: show fastfetch once in each interactive foot shell.
# Sourced from ~/.bashrc. Other terminals and non-interactive shells stay quiet.
if [[ $- == *i* ]] && [[ ${TERM:-} == foot* ]] && [[ -z ${RUSTED_FASTFETCH_SHOWN:-} ]] && command -v fastfetch >/dev/null 2>&1; then
    export RUSTED_FASTFETCH_SHOWN=1
    fastfetch --config "${XDG_CONFIG_HOME:-$HOME/.config}/fastfetch/config.jsonc" 2>/dev/null
fi
