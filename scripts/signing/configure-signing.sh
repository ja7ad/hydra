#!/usr/bin/env bash
set -euo pipefail
if [ -z "$SIGNPATH_API_TOKEN" ]; then
  echo '::error::Configure the SIGNPATH_API_TOKEN repository secret.'
  exit 1
fi
if [ -z "$HYDRA_PLUGIN_SIGNING_KEY" ]; then
  echo '::error::Configure the HYDRA_PLUGIN_SIGNING_KEY repository secret.'
  exit 1
fi
case "$SIGNING_POLICY" in
  test-signing)
    CERTIFICATE_THUMBPRINT=4924FFE138E36949CA3EDDF03252903CDB65B2AE
    ;;
  release-signing)
    if [ "$RELEASE_ENABLED" != true ]; then
      echo '::error::Production signing requires SIGNPATH_RELEASE_SIGNING_ENABLED=true after Foundation activation.'
      exit 1
    fi
    CERTIFICATE_THUMBPRINT="$RELEASE_THUMBPRINT"
    if [ "$CERTIFICATE_THUMBPRINT" = 4924FFE138E36949CA3EDDF03252903CDB65B2AE ]; then
      echo '::error::The test certificate cannot be used for production signing.'
      exit 1
    fi
    ;;
  *) echo '::error::Unsupported signing policy.'; exit 1 ;;
esac
if [[ ! "$CERTIFICATE_THUMBPRINT" =~ ^[[:xdigit:]]{40}$ ]]; then
  echo '::error::Set SIGNPATH_CERTIFICATE_THUMBPRINT to the issued release certificate thumbprint (40 hex characters).'
  exit 1
fi
{
  echo "signing_policy=$SIGNING_POLICY"
  echo "certificate_thumbprint=$CERTIFICATE_THUMBPRINT"
} >> "$GITHUB_OUTPUT"
