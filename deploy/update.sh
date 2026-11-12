#!/usr/bin/env bash
# Brings the server up to date with GitHub: pulls the code and the image of the new commit (built by
# .github/workflows/image.yml; only the layers that changed are downloaded), or builds it here with COTA_BUILD=1.
# As root or as cota.
#
#   bash /srv/cota/deploy/update.sh
set -euo pipefail
DIR=${DIR:-/srv/cota}
ENV_FILE=/etc/cota/cota.env
if [[ $EUID == 0 ]]; then cd /; exec runuser -u cota -- "$(realpath "$0")" "$@"; fi
cd "$DIR"
git pull -q --ff-only
git log --oneline -1
tag=$(deploy/image-tag.sh)
image=$(grep "^COTA_IMAGE=" "$ENV_FILE" | cut -d= -f2)
if [[ ${COTA_BUILD:-0} == 1 ]]; then
  COTA_IMAGE_TAG=$tag docker compose --env-file "$ENV_FILE" -f deploy/compose.yml build -q aggregate
elif ! docker pull -q "$image:$tag"; then
  echo "não consegui baixar $image:$tag. Ou a rede falhou (veja o erro acima), ou a imagem desta versão ainda está" \
    "sendo montada no GitHub (workflow \"Imagem do servidor\", ~15 min depois do push). As rodadas seguem com a" \
    "imagem anterior; rode de novo depois." >&2
  exit 1
fi
sed -i "s|^COTA_IMAGE_TAG=.*|COTA_IMAGE_TAG=$tag|" "$ENV_FILE"
docker image prune -f >/dev/null
echo "atualizado: $image:$tag"
