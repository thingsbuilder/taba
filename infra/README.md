# Infraestrutura

Tudo é feito com Ansible, em `infra/ansible/`:

| Playbook | O que faz | Onde roda |
|---|---|---|
| `provision.yml` | APIs, banco Firestore, índices e políticas de TTL (`firestore/firestore.indexes.json`), conta de serviço da VM, IP estático, firewall e VM e2-micro no GCP | Na sua máquina, contra a API do GCP |
| `configure.yml` | Atualizações automáticas, swap de 1 GB, Caddy (HTTPS automático), o arquivo de ambiente com os segredos e a release do Taba (binário, PWA e serviço systemd) | Na VM, por SSH |

Os dois são idempotentes: rodar de novo aplica só o que mudou.

## Segredos e valores locais

Este repositório é público. Os valores reais de cada instalação ficam em `infra/ansible/inventory/prod/`, que está no `.gitignore`:

```
inventory/
  example/   ← no git: modelo com valores de exemplo
  prod/      ← fora do git: os seus valores e o seu vault
```

## Primeira instalação

1. Pré-requisitos na sua máquina:
   - `ansible` e `gcloud`;
   - no Python do Ansible: `pip install google-auth requests` (com pipx: `pipx inject ansible google-auth requests`);
   - `ansible-galaxy collection install -r infra/ansible/requirements.yml`.
2. Login no GCP: `gcloud auth login`. Crie um projeto com faturamento ativado; a camada gratuita exige.
3. Monte o seu inventário:
   ```
   cd infra/ansible
   cp -r inventory/example inventory/prod
   cp inventory/prod/group_vars/all/vault.yml.example inventory/prod/group_vars/all/vault.yml
   ```
   Edite `inventory/prod/group_vars/all/vars.yml` e `vault.yml`, e criptografe o vault:
   `ansible-vault encrypt inventory/prod/group_vars/all/vault.yml`.
4. Aponte para a senha do vault: `export ANSIBLE_VAULT_PASSWORD_FILE=<arquivo com a senha>`.
5. Provisione: `ansible-playbook playbooks/provision.yml`. Ao final, ele mostra o IP da VM.
6. Crie no seu provedor de DNS um registro **A** do seu domínio para esse IP.
7. Configure a VM e instale a primeira release (ver "Deploy de uma versão"): `ansible-playbook playbooks/configure.yml -e taba_version=v0.1.0`.

## Deploy de uma versão

O binário e a PWA são compilados no GitHub Actions, nunca na VM (a e2-micro tem 1 GB de RAM).

1. Com a `main` verde no CI, crie e publique a tag: `git tag v0.1.0 && git push origin v0.1.0`.
2. O `release.yml` compila `taba` (`x86_64-unknown-linux-gnu`) e empacota a PWA. A release do GitHub recebe `taba-x86_64-unknown-linux-gnu`, `taba-pwa.tar.gz` e `SHA256SUMS`.
   O runner é o `ubuntu-24.04` (glibc 2.39) e o workflow falha se o binário pedir glibc maior que a do Debian 13 da VM (2.41).
3. Instale: `ansible-playbook playbooks/configure.yml -e taba_version=v0.1.0`. O playbook baixa os arquivos da release (conferindo o `SHA256SUMS`), instala o binário em `/usr/local/bin/taba`, descompacta a PWA em `/srv/taba/pwa`, instala `taba.service` e reinicia o serviço só se o binário, a unit ou o `/etc/taba/env` mudaram.
   Sem `-e`, vale o `taba_version` do `inventory/prod/group_vars/all/vars.yml`.
   **Instalação existente:** `inventory/prod` foi copiado antes destas variáveis existirem. Acrescente `taba_release_repo` (e `taba_version`) ao seu `vars.yml`, copiando de `inventory/example`; sem elas o `configure.yml` falha.
   **Fase 1 (identidade):** a API passa a exigir `HMAC_KEY` e `DOMAIN`. Antes do primeiro deploy com ela, acrescente ao `vars.yml` do `inventory/prod` a linha `hmac_key: "{{ vault_taba_hmac_key }}"` e ao vault `vault_taba_hmac_key` (gere com `openssl rand -base64 32`). **Guarde uma cópia da chave fora da VM e do repositório:** perdê-la, ou trocá-la, torna todos os IDs de voluntários e pessoas incomparáveis com os tokens novos. `unico_url` e `session_days` são opcionais.
   **Fase 2 (abrigo e rede):** a API passa a consultar abrigos, vínculos e avisos por índices compostos, e o Firestore recusa a consulta se o índice não existe. **Rode `provision.yml` antes do `configure.yml` da versão nova** para criar os índices e a política de TTL de `idempotency.expires_at`, e espere os índices ficarem prontos (console do Firestore, aba Índices; costuma levar alguns minutos). `shelter_radius_m` (padrão 100) e `event_radius_km` (padrão 50) são opcionais no `vars.yml`.
   **Fase 3 (check-in):** a API passa a exigir `THUMBNAIL_KEY` e mostra `PILOT_RESPONSIBLE` nas telas de coleta. Antes do deploy, acrescente ao `vars.yml` do `inventory/prod` as linhas `thumbnail_key: "{{ vault_taba_thumbnail_key }}"` e `pilot_responsible: "<nome e contato>"`, e ao vault `vault_taba_thumbnail_key` (gere com `openssl rand -base64 32`). Rode `provision.yml` antes, para os índices novos de `people` e `cards`. O índice de `households.member_ids` (array) é de campo único: o Firestore o cria sozinho, e a API do Firestore recusa um índice composto de um campo só.
4. Confira: `https://<seu domínio>/v1/status` deve responder `{"unico":"ok"}`.

Para voltar a uma versão anterior, rode o mesmo comando com a tag antiga.

### Como o app atualiza no celular

O Caddy serve `index.html`, `sw.js` e o manifesto com `Cache-Control: no-cache`; só `/assets/*` (nomes com hash) fica em cache por um ano. Quando há versão nova, o service worker novo instala em segundo plano e o app mostra "Nova versão disponível · Toque para atualizar". Só o toque ativa a versão nova e recarrega, para não recarregar no meio de um cadastro. O app procura versão nova ao abrir, ao voltar para o primeiro plano e a cada hora.
O deploy não apaga os arquivos de versões anteriores em `/srv/taba/pwa`: um app aberto ainda pode pedir os assets antigos até atualizar.

### Fontes

As fontes (Outfit, Figtree e IBM Plex Mono, licença SIL OFL 1.1) vão dentro da PWA, a partir dos pacotes `@fontsource/*`; as licenças ficam em `pwa/public/licenses/`. A CSP é `default-src 'self'` e o app abre offline, então o `@import` do Google Fonts de `design/tokens.css` é removido na build (`pwa/vite.config.ts`).

### Checagem de segredos

`scripts/check-secrets.sh repo` e `scripts/check-secrets.sh dir pwa/dist` (rodados no CI e no `release.yml`) falham se acharem chave `uk_live_…`, endereço IP, ID de projeto GCP (nome seguido de hífen e 6 dígitos) ou e-mail de conta de serviço. Na sua máquina, o `repo` também procura o `gcp_project` real do `inventory/prod`.

## Papéis globais (`validator`, `civil_defense`)

Só o operador da instância concede e retira, na VM, com o mesmo ambiente do serviço:

```
sudo sh -c 'set -a; . /etc/taba/env; exec /usr/local/bin/taba role grant <volunteer_id> civil_defense'
```

`grant` ou `revoke`, e `validator` ou `civil_defense`. O `volunteer_id` aparece em `GET /v1/me` da própria pessoa. Cada mudança grava `audit_log`. `mediator` e `protection` são concedidos pelo app (fase 4).

## Índices e TTL do Firestore

`firestore/firestore.indexes.json` usa o formato do Firebase CLI: `indexes` (índices compostos) e `fieldOverrides` (um item com `"ttl": true` vira política de TTL). O `provision.yml` aplica os dois pela API REST e pode ser repetido. As fases só acrescentam entradas ao arquivo. Hoje há TTL em `sessions.expires_at`, `captures.expires_at` e `idempotency.expires_at`; o do `audit_log` (1 ano) entra na fase 6. A fase 2 acrescenta os índices de `shelters` (`status` + `geohash`, `event_id` + `status`), `memberships` (`volunteer_id` + `status`, `shelter_id` + `status`) e `alerts` (`resolved` + `informational` + `created_at`). O emulador do Firestore não exige índices, então os testes não avisam de um índice faltando.

## Custo

Tudo fica na camada gratuita do GCP, menos o IPv4 externo (cerca de US$ 3 por mês). A camada gratuita da e2-micro só vale em `us-east1`, `us-central1` e `us-west1` (us-east1 é a mais perto do Brasil), e inclui 1 GB de saída por mês.
