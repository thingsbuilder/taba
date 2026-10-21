# Infraestrutura

Tudo é feito com Ansible, em `infra/ansible/`:

| Playbook | O que faz | Onde roda |
|---|---|---|
| `provision.yml` | APIs, banco Firestore, conta de serviço da VM, IP estático, firewall e VM e2-micro no GCP | Na sua máquina, contra a API do GCP |
| `configure.yml` | Atualizações automáticas, Caddy (HTTPS automático) e o arquivo de ambiente com os segredos | Na VM, por SSH |

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
7. Configure a VM: `ansible-playbook playbooks/configure.yml`.

## Custo

Tudo fica na camada gratuita do GCP, menos o IPv4 externo (cerca de US$ 3 por mês). A camada gratuita da e2-micro só vale em `us-east1`, `us-central1` e `us-west1` (us-east1 é a mais perto do Brasil), e inclui 1 GB de saída por mês.
