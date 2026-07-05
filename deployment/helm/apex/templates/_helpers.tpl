{{/*
Chart name, truncated/sanitized for use in Kubernetes object names.
*/}}
{{- define "apex.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Fully qualified app name: <release>-apex, or just the release name if it
already contains "apex" (standard Helm chart convention).
*/}}
{{- define "apex.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "apex.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels, applied to every object this chart renders. Deliberately does
NOT include the name/instance selector labels — every template composes
those separately via one of the *.selectorLabels helpers below, so including
them here too would set the same map keys twice in the same YAML document.
*/}}
{{- define "apex.labels" -}}
helm.sh/chart: {{ include "apex.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Selector labels for the `apex` server component specifically.
*/}}
{{- define "apex.selectorLabels" -}}
app.kubernetes.io/name: {{ include "apex.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "apex.server.selectorLabels" -}}
{{ include "apex.selectorLabels" . }}
app.kubernetes.io/component: server
{{- end -}}

{{- define "apex.postgres.selectorLabels" -}}
{{ include "apex.selectorLabels" . }}
app.kubernetes.io/component: postgres
{{- end -}}

{{- define "apex.qdrant.selectorLabels" -}}
{{ include "apex.selectorLabels" . }}
app.kubernetes.io/component: qdrant
{{- end -}}

{{/*
The postgres DSN apex-server/apex-cli expect, built from values.yaml's
postgres.* fields and this chart's postgres Service DNS name — used for both
APEX_MARKETPLACE_POSTGRES_URL and APEX_MEMORY_POSTGRES_URL (same database,
same convention deployment/docker-compose.yml uses).
*/}}
{{- define "apex.postgresDsn" -}}
{{- printf "postgres://%s:%s@%s-postgres:5432/%s" .Values.postgres.user .Values.postgres.password (include "apex.fullname" .) .Values.postgres.database -}}
{{- end -}}

{{- define "apex.qdrantUrl" -}}
{{- printf "http://%s-qdrant:6333" (include "apex.fullname" .) -}}
{{- end -}}
