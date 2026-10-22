# FR-001 — Criar painel de controle de administração

## Descrição

Uma área de administração no app que reúna a gestão de papéis, membros e abrigos e o estado da instalação, para que o dono e os administradores não precisem decorar IDs nem usar a CLI na VM.

## Problema atual

- Conceder `mediator` ou `protection` exige abrir `/roles` e colar o ID do voluntário (`volunteer_id`), que não é fácil de achar no app.
- Conceder `validator` ou `civil_defense` só é possível pela CLI na VM (`taba role grant <volunteer_id> <role>`).
- Não há uma tela única para ver quem tem qual papel, o estado da Unico, o evento ativo ou se o descarte está ligado. Isso hoje se confere por `/v1/status`, pelo log do serviço e pelo `vars.yml`.
- Descoberto no teste do dono em 2026-10-22, ao preparar a mediação entre dois aparelhos.

## Comportamento esperado

- Listar voluntários por nome (não por ID) e conceder ou revogar `mediator` e `protection` com um toque, por quem é `validator` ou Defesa Civil.
- Ver os papéis atuais de cada voluntário.
- Atalhos para membros e abrigos (confirmar, ver membros, encerrar evento).
- Ver o estado da instalação: Unico (ok ou degradado), evento ativo, descarte ligado ou desligado, responsável do piloto.
- Mostrar só o que o papel do usuário permite.

## Contexto técnico

- `POST /v1/volunteers/{id}/roles` e `DELETE /v1/volunteers/{id}/roles/{role}` já existem (fase 4). A tela `/roles` atual só os chama com o ID digitado.
- `validator` e `civil_defense` só têm CLI; levá-los ao app muda a regra da spec 2.1 e exige decisão do dono.
- `/v1/status` já devolve o estado da Unico e o `pilot_responsible`.
- Listar voluntários por nome é um dado pessoal: precisa de regra de visibilidade e de auditoria, e entra no RIPD.
- O painel do evento da fase 6 (`/dashboard`) é de contagens; este é de administração e não deve se misturar com ele.

## Escopo estimado

- Backend: rota de listagem de voluntários com busca por nome, restrita a `validator` e Defesa Civil, auditada; possível rota de estado da instalação.
- Frontend: nova tela de administração e ajuste de `/roles` (escolher da lista em vez de colar o ID).
- Docs: spec 2.1 e 8.x, RIPD (listagem de voluntários), fase nova ou extensão da fase 6.
- Decisão do dono: se `validator` e `civil_defense` passam a ser concedidos pelo app.
