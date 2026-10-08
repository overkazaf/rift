# Rift bash integration. Used as `bash --rcfile <this file>`; never edits user
# rc files. A --rcfile shell is not a login shell, so first emulate what a
# login shell would have read, then install the OSC 133 / OSC 7 hooks.

if [[ -r /etc/profile ]]; then source /etc/profile; fi
_rift_found=
for _rift_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
  if [[ -r $_rift_f ]]; then
    source "$_rift_f"
    _rift_found=1
    break
  fi
done
if [[ -z $_rift_found && -r $HOME/.bashrc ]]; then source "$HOME/.bashrc"; fi
unset _rift_found _rift_f

[[ $- == *i* ]] || return 0
[[ -t 1 ]] || return 0
[[ -n ${_RIFT_INTEGRATION_LOADED-} ]] && return 0
_RIFT_INTEGRATION_LOADED=1

_rift_osc7() {
  local LC_ALL=C str=$PWD out= c i
  for (( i = 0; i < ${#str}; i++ )); do
    c=${str:i:1}
    case $c in
      [A-Za-z0-9/._~-]) out+=$c ;;
      *) out+=$(printf '%%%02X' "'$c") ;;
    esac
  done
  printf '\e]7;file://%s%s\a' "${HOSTNAME:-localhost}" "$out"
}

# Runs FIRST in PROMPT_COMMAND: previous command finished (D), prompt starts (A).
_rift_precmd_first() {
  local s=$?
  _rift_in_prompt=1
  _rift_cmd_started=0
  if [[ -n ${_rift_prompted-} ]]; then printf '\e]133;D;%s\a' "$s"; fi
  _rift_prompted=1
  printf '\e]133;A\a'
  _rift_osc7
  return $s
}

# Runs LAST in PROMPT_COMMAND (after starship & co. rebuilt PS1): append B.
_rift_precmd_last() {
  local s=$?
  case $PS1 in
    *'133;B'*) ;;
    *) PS1=$PS1'\[\e]133;B\a\]' ;;
  esac
  _rift_in_prompt=0
  return $s
}

# Hand-rolled PROMPT_COMMAND install (string form on bash < 5.1, array after).
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
  PROMPT_COMMAND=(_rift_precmd_first "${PROMPT_COMMAND[@]}" _rift_precmd_last)
else
  PROMPT_COMMAND="_rift_precmd_first"$'\n'"${PROMPT_COMMAND-}"$'\n'"_rift_precmd_last"
fi

# C (output starts): PS0 is printed after Enter, before execution (bash >= 4.4).
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
  PS0=${PS0-}'\e]133;C\a'
elif [[ -z $(trap -p DEBUG) ]]; then
  _rift_debug() {
    [[ ${_rift_in_prompt-0} == 1 || -n ${COMP_LINE-} ]] && return 0
    [[ $BASH_COMMAND == _rift_precmd_* ]] && return 0
    if [[ ${_rift_cmd_started-0} != 1 ]]; then
      _rift_cmd_started=1
      printf '\e]133;C\a'
    fi
    return 0
  }
  trap '_rift_debug' DEBUG
fi
