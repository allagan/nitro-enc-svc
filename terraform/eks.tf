# ── Control plane log group ───────────────────────────────────────────────────

resource "aws_cloudwatch_log_group" "eks_cluster" {
  name              = "/aws/eks/${var.cluster_name}/cluster"
  retention_in_days = 90

  tags = {
    Name = "${var.cluster_name}-control-plane-logs"
  }
}

# ── EKS Cluster ───────────────────────────────────────────────────────────────

resource "aws_eks_cluster" "main" {
  name     = var.cluster_name
  version  = var.cluster_version
  role_arn = aws_iam_role.eks_cluster.arn

  enabled_cluster_log_types = [
    "api",
    "audit",
    "authenticator",
    "controllerManager",
    "scheduler",
  ]

  vpc_config {
    subnet_ids              = aws_subnet.private[*].id
    endpoint_private_access = true
    endpoint_public_access  = true
    public_access_cidrs     = var.public_access_cidrs
  }

  encryption_config {
    provider {
      key_arn = aws_kms_key.eks_secrets.arn
    }
    resources = ["secrets"]
  }

  access_config {
    authentication_mode = "API_AND_CONFIG_MAP"
  }

  depends_on = [
    aws_iam_role_policy_attachment.eks_cluster_policy,
    aws_cloudwatch_log_group.eks_cluster,
  ]

  tags = {
    Name = var.cluster_name
  }
}

# ── General node group (system workloads) ─────────────────────────────────────

resource "aws_eks_node_group" "general" {
  cluster_name    = aws_eks_cluster.main.name
  node_group_name = "${var.cluster_name}-general"
  node_role_arn   = aws_iam_role.general_node.arn
  subnet_ids      = aws_subnet.private[*].id

  instance_types = var.general_instance_types

  scaling_config {
    desired_size = var.general_desired_size
    min_size     = var.general_min_size
    max_size     = var.general_max_size
  }

  update_config {
    max_unavailable = 1
  }

  taint {
    key    = "CriticalAddonsOnly"
    value  = "true"
    effect = "NO_SCHEDULE"
  }

  depends_on = [
    aws_iam_role_policy_attachment.general_node_worker,
    aws_iam_role_policy_attachment.general_node_cni,
    aws_iam_role_policy_attachment.general_node_ecr,
  ]

  tags = {
    Name = "${var.cluster_name}-general"
  }
}

# ── Karpenter (Helm) ──────────────────────────────────────────────────────────

resource "helm_release" "karpenter" {
  namespace        = "kube-system"
  name             = "karpenter"
  repository       = "oci://public.ecr.aws/karpenter"
  chart            = "karpenter"
  version          = var.karpenter_version
  create_namespace = false

  values = [yamlencode({
    settings = {
      clusterName     = aws_eks_cluster.main.name
      clusterEndpoint = aws_eks_cluster.main.endpoint
    }
    tolerations = [{
      key      = "CriticalAddonsOnly"
      operator = "Exists"
    }]
    affinity = {
      nodeAffinity = {
        requiredDuringSchedulingIgnoredDuringExecution = {
          nodeSelectorTerms = [{
            matchExpressions = [{
              key      = "kubernetes.io/os"
              operator = "In"
              values   = ["linux"]
            }]
          }]
        }
      }
    }
  })]

  depends_on = [
    aws_eks_node_group.general,
    aws_eks_addon.pod_identity_agent,
    aws_eks_pod_identity_association.karpenter,
  ]
}

# ── EKS access entry — enclave node role (allows Karpenter-launched nodes to join) ──

resource "aws_eks_access_entry" "enclave_node" {
  cluster_name  = aws_eks_cluster.main.name
  principal_arn = aws_iam_role.enclave_node.arn
  type          = "EC2_LINUX"

  depends_on = [aws_eks_cluster.main]
}

# ── EKS access entry — test CodeBuild role (kubectl view access) ──────────────

resource "aws_eks_access_entry" "codebuild_test" {
  cluster_name  = aws_eks_cluster.main.name
  principal_arn = aws_iam_role.codebuild_test.arn
  type          = "STANDARD"
}

resource "aws_eks_access_policy_association" "codebuild_test" {
  cluster_name  = aws_eks_cluster.main.name
  principal_arn = aws_iam_role.codebuild_test.arn
  policy_arn    = "arn:aws:eks::aws:cluster-access-policy/AmazonEKSViewPolicy"

  access_scope {
    type = "cluster"
  }

  depends_on = [aws_eks_access_entry.codebuild_test]
}

# ── EKS Pod Identity Agent (must be installed before addon Pod Identity associations) ──

resource "aws_eks_addon" "pod_identity_agent" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "eks-pod-identity-agent"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_node_group.general,
  ]

  tags = {
    Name = "${var.cluster_name}-pod-identity-agent"
  }
}

# ── Core addons ───────────────────────────────────────────────────────────────

resource "aws_eks_addon" "vpc_cni" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "vpc-cni"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_pod_identity_association.vpc_cni,
  ]

  tags = {
    Name = "${var.cluster_name}-vpc-cni"
  }
}

resource "aws_eks_addon" "coredns" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "coredns"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_node_group.general,
  ]

  tags = {
    Name = "${var.cluster_name}-coredns"
  }
}

resource "aws_eks_addon" "kube_proxy" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "kube-proxy"
  resolve_conflicts_on_update = "OVERWRITE"

  tags = {
    Name = "${var.cluster_name}-kube-proxy"
  }
}

resource "aws_eks_addon" "ebs_csi_driver" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "aws-ebs-csi-driver"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_pod_identity_association.ebs_csi,
    aws_eks_node_group.general,
  ]

  tags = {
    Name = "${var.cluster_name}-ebs-csi-driver"
  }
}

# ── Cluster admin access entries ──────────────────────────────────────────

resource "aws_eks_access_entry" "admin" {
  for_each = toset(var.cluster_admin_arns)

  cluster_name  = aws_eks_cluster.main.name
  principal_arn = each.key
  type          = "STANDARD"
}

resource "aws_eks_access_policy_association" "admin" {
  for_each = toset(var.cluster_admin_arns)

  cluster_name  = aws_eks_cluster.main.name
  principal_arn = each.key
  policy_arn    = "arn:aws:eks::aws:cluster-access-policy/AmazonEKSClusterAdminPolicy"

  access_scope {
    type = "cluster"
  }

  depends_on = [aws_eks_access_entry.admin]
}

# ── Pod Identity associations ─────────────────────────────────────────────────

resource "aws_eks_pod_identity_association" "vpc_cni" {
  cluster_name    = aws_eks_cluster.main.name
  namespace       = "kube-system"
  service_account = "aws-node"
  role_arn        = aws_iam_role.vpc_cni_pod_identity.arn

  depends_on = [aws_eks_addon.pod_identity_agent]
}

resource "aws_eks_pod_identity_association" "ebs_csi" {
  cluster_name    = aws_eks_cluster.main.name
  namespace       = "kube-system"
  service_account = "ebs-csi-controller-sa"
  role_arn        = aws_iam_role.ebs_csi_pod_identity.arn

  depends_on = [aws_eks_addon.pod_identity_agent]
}
