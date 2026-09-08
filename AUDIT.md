# Audit local — 2026-09-05

## Suivi du correctif — 2026-09-08

Stela utilise désormais `lithair-core` et `lithair-macros` 1.10.0 depuis
crates.io, sans patch local. Le rebuild compare les assets persistés aux
URLs des posts/pages publiés et supprime les anciennes URLs via
`delete_asset`. Le CSS, le RSS, l'index et la page 404 sont conservés.

**Le défaut d'asset de lithair#227 et la dépublication sont corrigés.**
Une sonde Rust isolée a vérifié directement la suppression effective,
l'idempotence, les compteurs, la relecture du store, la recréation au même
chemin et une seconde relecture. Une sonde HTTP a vérifié pour les posts
et les pages : publication → brouillon via PUT → rebuild automatique → 404,
rebuild explicite → 404, redémarrage → 404, republication → 200.
Un post non concerné et les assets du thème restent accessibles.

**Un second défaut Lithair reste présent sur DELETE des modèles.**
Pour les posts comme pour les pages, DELETE répond 204 et déclenche bien
le hook, mais le contenu revient dans le rebuild et après redémarrage
(GET de l'URL publique → 200). Dans le code publié 1.10.0,
`src/http/declarative.rs`, `replay_events()` (lignes 507–512) désérialise
chaque payload puis l'insère sans examiner `event_type`, alors que
`handle_delete()` persiste le dernier objet sous un événement `Deleted`.
Ce défaut est distinct des tombstones du moteur frontend, qui fonctionnent.
Le correctif appartient au replay des modèles Lithair ; aucun contournement
du journal n'est ajouté à Stela.

Validation de cette mise à jour :

- Compilation native et clippy avec `-D warnings` : réussis.
- 7 tests Rust : réussis, dont la classification des URLs à réconcilier.
- 63 contrôles probatum : réussis, preuves dans `.probatum/runs/0069`.
  Configuration temporaire adaptée aux chemins/ports locaux et au binaire
  debug ; le manifeste du dépôt reste inchangé.
- 8 tests Playwright : réussis sous Chromium, dont le nouveau parcours de
  dépublication. Configuration temporaire avec blog/port isolés et binaire
  debug ; les assertions sont celles de la suite du dépôt.
- Sondes moteur et HTTP : résultats détaillés ci-dessus.

Probatum 0.9 ne sait exprimer que GET/POST ; les sondes PUT/DELETE ont donc
été exécutées séparément. Le parcours navigateur « publié → brouillon »
est ajouté à `e2e/editor.spec.js` pour couvrir le formulaire, son PUT et son
rebuild. La construction musl et la pipeline cidx complète n'ont pas été
relancées pour cette vérification locale.

Les constats du 2026-09-05 ci-dessous sont conservés comme historique.

### Intégration durable des sondes dans probatum

La sonde Python est désormais conservée dans `tests/asset_lifecycle.py`,
paramétrable avec `--binary` et `--scenario`. Le manifeste principal exécute
le scénario de dépublication. cidx copie le runner probatum 0.9 officiel puis
exécute la suite dans une image Python Alpine, sans dépendance Python dans
le produit ni installation pip.

Vérification sur le binaire musl reconstruit avec Lithair 1.10.0 :

- `cidx run cargo-build-musl` : réussi.
- `cidx run probatum-runner` puis `cidx run probatum` : **64 checks réussis**,
  preuves dans `.probatum/runs/0070`.
- `probatum run tests/probatum-delete.toml` : **échec confirmé**, attendu 404
  après DELETE, obtenu 200 ; preuves dans `.probatum/runs/0071`. Ce manifeste
  reste séparé de la suite verte, sans transformer l'échec en succès.
- `cidx validate` : réussi ; workflow régénéré sans différence.

Les scripts arrêtent leurs serveurs et nettoient leurs répertoires temporaires.
Le scénario complet et les commandes sont documentés dans `tests/README.md`.

Périmètre : code et configuration du checkout `3f6555f`, lecture de
`CLAUDE.md`, compilation et sondes HTTP sur un blog temporaire. Aucun état
de release, ticket distant ou déploiement n'a été vérifié. Le code applicatif
n'a pas été modifié pendant cet audit.

## 1. Priorité haute : dépublier laisse le contenu accessible

Dans `src/main.rs`, `rebuild()` filtre les contenus publiés puis ajoute ou
actualise leurs assets. Il ne retire jamais les anciennes URLs.

Reproduction sur le binaire recompilé, avec session authentifiée :

1. `POST /api/posts` avec un article `published: true` → 201.
2. `POST <prefix>/rebuild` → 200 ; `GET /posts/audit-post` → 200.
3. `PUT /api/posts/audit-post` avec `published: false` → 200.
4. Nouveau rebuild → 200 ; la même URL répond encore 200 avec le corps
   de l'article désormais brouillon.

Le filtrage de l'index ne suffit donc pas à retirer une publication. Le même
chemin de code expose les pages à ce défaut ; la suppression présente aussi
un risque identique, mais ces deux variantes n'ont pas été rejouées ici.

Point upstream concret : dans le code installé de **lithair-core 1.9.0**,
`FrontendEngine::delete_asset()` retourne `Ok(())` sans supprimer quoi que ce
soit (`src/frontend/engine.rs`). Ajouter un appel à cette méthode ne suffit
pas. Prévoir une suppression effective côté moteur puis la réconciliation
des assets côté Stela, avec vérification de la persistance au redémarrage.
La régression HTTP appartient à probatum.

Suivi upstream : [lithair#227](https://github.com/lithair/lithair/issues/227),
publiée le 2026-09-05 après vérification du code de la branche par défaut
et recherche de doublons. L'issue porte sur la suppression effective côté
Lithair ; la réconciliation des assets reste à implémenter dans Stela.

## 2. Priorité moyenne : l'API ne permet pas la lecture headless annoncée

Après connexion réussie, `GET /api/posts` retourne 404 alors que le POST
fonctionne. La route GET `/*` de Stela envoie la requête au serveur d'assets.
Ce défaut est déjà reconnu dans `CLAUDE.md`, mais le README invite toujours
à connecter Astro à l'API.

Il faut traiter ensemble l'ordre/routage des requêtes et le contrat d'accès :
les modèles exigent actuellement une session, y compris en lecture, et les
brouillons ne doivent pas être exposés par une simple ouverture des GET.

## 3. Priorité moyenne : le flux RSS accepte des URLs non échappées

`SiteSettings.base_url` et la configuration n'ont pas de validation d'URL.
`theme/rss.xml` injecte cette valeur et les URLs dérivées avec `| safe`,
y compris dans l'attribut `href` du lien Atom.

Reproduction : enregistrer les settings avec
`base_url = "https://example.test/?a=1&b=2"` → 201 ; rebuild → succès ;
le flux obtenu échoue au parsing XML (`invalid token`). La présence d'une
query illustre aussi l'absence de contrat sur ce qu'une URL de base accepte.

Définir et valider ce contrat, puis échapper les valeurs dans leur contexte
XML. Valider le slug seul ne sécurise pas une URL contenant une base libre.
Un `/` encodé en entité reste valide en XML : éviter de confondre lisibilité
de la source et nécessité de désactiver l'échappement.

## 4. Documentation et limites du produit

- `CLAUDE.md` indique encore « design phase » et contient des chemins auth
  historiques ; le serveur utilise désormais le préfixe personnalisé.
- Les thèmes sont embarqués par `include_str!`, pas chargés depuis un dossier
  au rebuild. Modifier un fichier nécessite de recompiler le binaire.
- L'éditeur Stela est à `<prefix>`, le dashboard Lithair à `<prefix>/panel` :
  la décision « éditeur comme onglet du dashboard » n'est pas réalisée.
- L'éditeur propose les posts, mais pas de formulaire pour pages ou settings.
- Les champs de présentation du fichier de configuration priment sur les
  flags CLI correspondants. Le comportement mérite une décision explicite.
- Les dates RSS sont celles du rebuild et le tri des posts est alphabétique
  par slug ; ce sont des limites connues, pas un historique de publication.

`AGENTS.md` distingue désormais ces états réels des intentions historiques.

## Vérifications effectuées

- `cargo test --locked --offline` : 6 tests réussis.
- `cargo fmt --check` : réussi.
- `cargo build --locked --offline` : réussi.
- Sondes HTTP isolées : publication/dépublication, lecture API authentifiée,
  écriture des settings et parsing XML du RSS, résultats ci-dessus.

Le sandbox interdisait l'écoute réseau ; les sondes ont été exécutées avec
autorisation sur `127.0.0.1:3397`, dans un répertoire temporaire nettoyé après
arrêt du serveur. Le client Python a transmis explicitement le cookie de
session : il omettait sinon le cookie `Secure` sur HTTP. Ces sondes ne
constituent pas une validation du comportement d'un navigateur.

Les suites cidx, probatum et Playwright complètes, les scans de dépendances
et la construction d'image n'ont pas été exécutés. Les tests Rust actuels
ne couvrent pas les transitions de publication ; leur succès n'invalide pas
le premier constat.

Suite proposée : corriger et couvrir la dépublication avec Lithair, puis
le RSS, puis clarifier le contrat headless et aligner la documentation produit.
