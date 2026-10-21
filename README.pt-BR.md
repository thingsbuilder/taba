# Taba

Português · [English](README.md)

O **Taba** é uma rede open source de abrigos para desastres no Brasil, como enchentes. Quem perde a casa costuma perder os documentos também. O Taba reconhece as pessoas pelo rosto, sem documento, para que os abrigos funcionem como uma rede só:

- saber quantas pessoas **únicas** estão abrigadas, e não quantos cadastros existem;
- evitar cadastro duplicado e retirada de doação repetida entre abrigos;
- ajudar **famílias a encontrar parentes desaparecidos**, inclusive quem não consegue dizer o próprio nome.

> **Situação:** fase de especificação. Nada foi implementado ainda. Veja a [especificação](docs/pt_br/spec/).

## Como funciona

O Taba usa o [Human Token da Unico](https://developer.unico.io/human-token/index.html). A partir de uma selfie, o Human Token devolve um identificador estável e opaco: a mesma pessoa sempre recebe o mesmo token. O token **não diz quem a pessoa é**, só que ela é a mesma de antes.

Principais funcionalidades:

1. **Check-in em abrigos** por selfie, sem duplicidade entre abrigos.
2. **Doações** com detecção de duplicidade. A retirada repetida é sinalizada para revisão, nunca recusada na hora.
3. **Registro de resgatados** no ponto de desembarque ou triagem.
4. **Voluntários**, convidados por QR code ou link, sempre verificados com Human Token.
5. **Busca de desaparecidos** a partir de fotos enviadas pela família.
6. **"Estou Seguro"**: o próprio morador se registra. Quem precisa de resgate é orientado a ligar 193 (Bombeiros) ou 199 (Defesa Civil).
7. **Revalidação** em doações de alto valor e na saída de uma criança do abrigo.

Um abrigo pode ser criado por um único operador com um celular. O app funciona sem internet e sincroniza quando houver sinal. O abrigo novo nasce **não confirmado** e só enxerga os próprios dados até ser confirmado. Os abrigos formam **eventos** automaticamente, pela distância entre eles, sem ninguém precisar declarar num backoffice que um desastre começou.

**Stack:** aplicação web instalável (PWA) em TypeScript, API em Rust, Firestore e uma VM e2-micro no GCP provisionada com Ansible ([infra/README.md](infra/README.md)).

## Questões legais

O Taba trata **dados biométricos de pessoas vulneráveis**. Quem rodar uma instância precisa levar isso a sério.

- **Biometria é dado pessoal sensível** pela LGPD (art. 5º, II). Cada operação tem uma base legal no art. 11. Por exemplo: proteção da vida na busca de desaparecidos, prevenção à fraude nas doações, política pública na operação do poder público. Os detalhes estão em [base legal LGPD](docs/pt_br/base%20legal%20LGPD.md).
- **Só o token é guardado, nunca a foto.** E o token é guardado como HMAC, para que um banco vazado não possa ser cruzado com outros sistemas. A única exceção é uma miniatura do rosto, criptografada, de quem está sem identificação, como uma pessoa inconsciente ou uma criança pequena. Ela é apagada assim que a pessoa tiver um nome ou um número de pulseira.
- **A busca nunca revela a localização para quem buscou.** Um mediador (operador de abrigo ou Defesa Civil), ou a própria pessoa encontrada, decide se o contato vai acontecer. Isso protege vítimas de agressores que estejam procurando por elas.
- **Os dados são apagados quando o evento termina**, depois de um prazo de retenção.
- **Crianças** seguem o art. 14 da LGPD (melhor interesse da criança).
- **Localização dos dados.** A instância de referência roda nos Estados Unidos, o que é transferência internacional de dados (LGPD, art. 33). Cada instalação escolhe a própria região: uma prefeitura pode rodar a sua instância em São Paulo.
- **Controlador.** Quem roda uma instância num desastre real, como uma prefeitura ou uma organização humanitária, é o controlador dos dados. Essa organização precisa do próprio relatório de impacto (RIPD) e da própria análise jurídica.
- **Unico.** Cada instalação precisa da própria chave de acesso da Unico e deve seguir os termos de uso dela.

Isto não é parecer jurídico. Valide com um advogado ou com o encarregado de dados (DPO) antes de usar o Taba com dados reais.

## A origem do nome

*Taba* é uma palavra do **tupi**, língua indígena do Brasil. Significa **aldeia**: o conjunto de *ocas* (casas) onde uma comunidade vive junta. Do mesmo jeito, um evento de desastre no Taba é o conjunto de abrigos que funcionam como uma rede só.

Como muitas palavras de origem tupi, *taba* faz parte do português do dia a dia. Usamos o nome com respeito aos povos indígenas cuja língua o deu para nós.

## Como contribuir

- Código, identificadores, rotas da API, mensagens de commit e pull requests são em **inglês**.
- A documentação é em português ([docs/pt_br](docs/pt_br/)), com uma versão da especificação em inglês ([docs/en](docs/en/)).
- Nunca commite segredos nem dados reais. Cada instalação guarda os próprios valores em `infra/ansible/inventory/prod/` e no `.env`, ambos ignorados pelo git.

## Licença

[GNU Affero General Public License v3.0](LICENSE). Se você rodar uma versão modificada do Taba como serviço, precisa publicar as suas mudanças.
