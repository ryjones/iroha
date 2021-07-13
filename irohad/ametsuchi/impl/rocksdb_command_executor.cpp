/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#include "ametsuchi/impl/rocksdb_command_executor.hpp"

#include <fmt/core.h>
#include <rocksdb/utilities/transaction.h>
#include <boost/algorithm/string.hpp>
#include <boost/variant/apply_visitor.hpp>
#include "ametsuchi/impl/executor_common.hpp"
#include "ametsuchi/setting_query.hpp"
#include "ametsuchi/vm_caller.hpp"
#include "interfaces/commands/add_asset_quantity.hpp"
#include "interfaces/commands/add_peer.hpp"
#include "interfaces/commands/add_signatory.hpp"
#include "interfaces/commands/append_role.hpp"
#include "interfaces/commands/call_engine.hpp"
#include "interfaces/commands/command.hpp"
#include "interfaces/commands/compare_and_set_account_detail.hpp"
#include "interfaces/commands/create_account.hpp"
#include "interfaces/commands/create_asset.hpp"
#include "interfaces/commands/create_domain.hpp"
#include "interfaces/commands/create_role.hpp"
#include "interfaces/commands/detach_role.hpp"
#include "interfaces/commands/grant_permission.hpp"
#include "interfaces/commands/remove_peer.hpp"
#include "interfaces/commands/remove_signatory.hpp"
#include "interfaces/commands/revoke_permission.hpp"
#include "interfaces/commands/set_account_detail.hpp"
#include "interfaces/commands/set_quorum.hpp"
#include "interfaces/commands/set_setting_value.hpp"
#include "interfaces/commands/subtract_asset_quantity.hpp"
#include "interfaces/commands/transfer_asset.hpp"

using namespace iroha;
using namespace iroha::ametsuchi;

using shared_model::interface::permissions::Grantable;
using shared_model::interface::permissions::Role;

using shared_model::interface::GrantablePermissionSet;
using shared_model::interface::RolePermissionSet;

RocksDbCommandExecutor::RocksDbCommandExecutor(
    std::shared_ptr<RocksDBContext> db_context,
    std::shared_ptr<shared_model::interface::PermissionToString> perm_converter,
    std::optional<std::reference_wrapper<const VmCaller>> vm_caller)
    : db_context_(std::move(db_context)),
      perm_converter_{std::move(perm_converter)},
      vm_caller_{vm_caller},
      db_transaction_(db_context_) {
  assert(db_context_);
}

RocksDbCommandExecutor::~RocksDbCommandExecutor() = default;

void RocksDbCommandExecutor::skipChanges() {
  RocksDbCommon common(db_context_);
  common.skip();
}

DatabaseTransaction &RocksDbCommandExecutor::dbSession() {
  return db_transaction_;
}

std::shared_ptr<RocksDBContext> RocksDbCommandExecutor::getSession() {
  return db_context_;
}

CommandResult RocksDbCommandExecutor::execute(
    const shared_model::interface::Command &cmd,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType cmd_index,
    bool do_validation) {
  return boost::apply_visitor(
      [this, &creator_account_id, &tx_hash, cmd_index, do_validation](
          const auto &command) -> CommandResult {
        // TODO(iceseer): remove try-catch when commands will be implemented
        try {
          RolePermissionSet creator_permissions;
          RocksDbCommon common(db_context_);
          if (do_validation) {
            auto const &[account_name, domain_id] =
                staticSplitId<2ull>(creator_account_id);

            // get account permissions
            if (auto result =
                    accountPermissions(common, account_name, domain_id);
                expected::hasError(result))
              return expected::makeError(
                  CommandError{command.toString(),
                               result.assumeError().code,
                               result.assumeError().description});
            else
              creator_permissions = result.assumeValue();
          }

          if (auto result = (*this)(common,
                                    command,
                                    creator_account_id,
                                    tx_hash,
                                    cmd_index,
                                    do_validation,
                                    creator_permissions);
              expected::hasError(result))
            return expected::makeError(
                CommandError{command.toString(),
                             result.assumeError().code,
                             fmt::format("Command: {}. {}",
                                         command.toString(),
                                         result.assumeError().description)});

          return {};
        } catch (std::exception &e) {
          return expected::makeError(CommandError{
              command.toString(), ErrorCodes::kException, e.what()});
        }
      },
      cmd.get());
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::AddAssetQuantity &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[asset_name, domain_id] = staticSplitId<2>(command.assetId());
  auto const &amount = command.amount();

  if (do_validation)
    RDB_ERROR_CHECK(checkPermissions(domain_id,
                                     creator_domain_id,
                                     creator_permissions,
                                     Role::kAddAssetQty,
                                     Role::kAddDomainAssetQty));

  // check if asset exists and construct amount by precision
  RDB_TRY_GET_VALUE(asset_amount,
                    forAsset<kDbOperation::kGet, kDbEntry::kMustExist>(
                        common, asset_name, domain_id));
  shared_model::interface::Amount result(*asset_amount);

  RDB_TRY_GET_VALUE(
      account_asset_sz,
      forAccountAssetSize<kDbOperation::kGet, kDbEntry::kCanExist>(
          common, creator_account_name, creator_domain_id));
  uint64_t account_asset_size(account_asset_sz ? *account_asset_sz : 0ull);

  {  // get account asset balance
    RDB_TRY_GET_VALUE(balance,
                      forAccountAsset<kDbOperation::kGet, kDbEntry::kCanExist>(
                          common,
                          creator_account_name,
                          creator_domain_id,
                          command.assetId()));
    if (!balance)
      ++account_asset_size;
    else
      result = std::move(*balance);
  }

  result += amount;
  common.valueBuffer().assign(result.toStringRepr());
  if (common.valueBuffer()[0] == 'N')
    return makeError<void>(ErrorCodes::kInvalidAssetAmount,
                           "Invalid asset {} amount {}",
                           command.assetId(),
                           result.toString());

  RDB_ERROR_CHECK(forAccountAsset<kDbOperation::kPut>(
      common, creator_account_name, creator_domain_id, command.assetId()));

  common.encode(account_asset_size);
  RDB_ERROR_CHECK(forAccountAssetSize<kDbOperation::kPut>(
      common, creator_account_name, creator_domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::AddPeer &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &peer = command.peer();
  if (do_validation)
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kAddPeer));

  RDB_ERROR_CHECK(forPeerAddress<kDbOperation::kCheck, kDbEntry::kMustNotExist>(
      common, peer.pubkey()));

  RDB_TRY_GET_VALUE(
      opt_peers_count,
      forPeersCount<kDbOperation::kGet, kDbEntry::kCanExist>(common));

  common.encode((opt_peers_count ? *opt_peers_count : 0ull) + 1ull);
  RDB_ERROR_CHECK(forPeersCount<kDbOperation::kPut>(common));

  /// Store address
  common.valueBuffer().assign(peer.address());
  RDB_ERROR_CHECK(forPeerAddress<kDbOperation::kPut>(common, peer.pubkey()));

  /// Store TLS if present
  if (peer.tlsCertificate().has_value()) {
    common.valueBuffer().assign(peer.tlsCertificate().value());
    RDB_ERROR_CHECK(forPeerTLS<kDbOperation::kPut>(common, peer.pubkey()));
  }

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::AddSignatory &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());

  if (do_validation) {
    GrantablePermissionSet granted_account_permissions;
    RDB_TRY_GET_VALUE(
        opt_permissions,
        forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
            common,
            creator_account_name,
            creator_domain_id,
            account_name,
            domain_id));
    if (opt_permissions)
      granted_account_permissions = *opt_permissions;

    if (creator_account_id == command.accountId()) {
      RDB_ERROR_CHECK(
          checkPermissions(creator_permissions, Role::kAddSignatory));
    } else {
      RDB_ERROR_CHECK(checkGrantablePermissions(creator_permissions,
                                                granted_account_permissions,
                                                Grantable::kAddMySignatory));
    }
  }

  RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
      common, account_name, domain_id));

  RDB_ERROR_CHECK(forSignatory<kDbOperation::kCheck, kDbEntry::kMustNotExist>(
      common, account_name, domain_id, command.pubkey()));

  common.valueBuffer().clear();
  RDB_ERROR_CHECK(forSignatory<kDbOperation::kPut>(
      common, account_name, domain_id, command.pubkey()));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::AppendRole &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());
  auto const &role_name = command.roleName();

  if (do_validation) {
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kAppendRole));

    RDB_TRY_GET_VALUE(
        opt_permissions,
        forRole<kDbOperation::kGet, kDbEntry::kMustExist>(common, role_name));
    if (!opt_permissions->isSubsetOf(creator_permissions))
      return makeError<void>(ErrorCodes::kNoPermissions,
                             "Insufficient permissions");
  }

  RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
      common, account_name, domain_id));

  // Account must not have role, else return error.
  RDB_ERROR_CHECK(forAccountRole<kDbOperation::kCheck, kDbEntry::kMustNotExist>(
      common, account_name, domain_id, role_name));

  common.valueBuffer() = "";
  RDB_ERROR_CHECK(forAccountRole<kDbOperation::kPut>(
      common, account_name, domain_id, role_name));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::CallEngine &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool /*do_validation*/,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  return makeError<void>(ErrorCodes::kNoImplementation, "Not implemented");
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::CompareAndSetAccountDetail &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());

  GrantablePermissionSet granted_account_permissions;
  RDB_TRY_GET_VALUE(
      opt_permissions,
      forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
          common,
          creator_account_name,
          creator_domain_id,
          account_name,
          domain_id));
  if (opt_permissions)
    granted_account_permissions = *opt_permissions;

  if (do_validation)
    RDB_ERROR_CHECK(checkPermissions(creator_permissions,
                                     granted_account_permissions,
                                     Role::kGetMyAccDetail,
                                     Grantable::kSetMyAccountDetail));

  std::string_view const creator_id = !creator_account_id.empty()
      ? creator_account_id
      : std::string_view{"genesis"};

  RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
      common, account_name, domain_id));

  RDB_TRY_GET_VALUE(
      opt_detail,
      forAccountDetail<kDbOperation::kGet, kDbEntry::kCanExist>(
          common, account_name, domain_id, creator_id, command.key()));

  bool const eq = (command.oldValue() && opt_detail)
      ? *opt_detail == *command.oldValue()
      : false;
  bool const same =
      command.checkEmpty() ? !command.oldValue() && !opt_detail : !opt_detail;

  if (eq || same) {
    RDB_TRY_GET_VALUE(
        opt_detail,
        forAccountDetail<kDbOperation::kGet, kDbEntry::kCanExist>(
            common,
            account_name,
            domain_id,
            !creator_account_id.empty() ? creator_account_id : "genesis",
            command.key()));

    common.valueBuffer().assign(command.value());
    RDB_ERROR_CHECK(forAccountDetail<kDbOperation::kPut>(
        common, account_name, domain_id, creator_id, command.key()));

    if (!opt_detail) {
      RDB_TRY_GET_VALUE(
          opt_acc_details_count,
          forAccountDetailsCount<kDbOperation::kGet, kDbEntry::kCanExist>(
              common, account_name, domain_id));
      const uint64_t count =
          opt_acc_details_count ? *opt_acc_details_count : 0ull;

      common.encode(count + 1ull);
      RDB_ERROR_CHECK(forAccountDetailsCount<kDbOperation::kPut>(
          common, account_name, domain_id));
    }

    return {};
  }

  return makeError<void>(ErrorCodes::kIncorrectOldValue, "Old value incorrect");
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::CreateAccount &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &account_name = command.accountName();
  auto const &domain_id = command.domainId();
  auto pubkey = command.pubkey();
  boost::algorithm::to_lower(pubkey);

  if (do_validation)
    RDB_ERROR_CHECK(
        checkPermissions(creator_permissions, Role::kCreateAccount));

  // check if domain exists
  RDB_TRY_GET_VALUE(
      opt_default_role,
      forDomain<kDbOperation::kGet, kDbEntry::kMustExist>(common, domain_id));
  std::string default_role(*opt_default_role);

  RDB_TRY_GET_VALUE(
      opt_permissions,
      forRole<kDbOperation::kGet, kDbEntry::kMustExist>(common, default_role));

  if (do_validation && !opt_permissions->isSubsetOf(creator_permissions))
    return makeError<void>(ErrorCodes::kNoPermissions,
                           "Insufficient permissions");

  common.valueBuffer() = "";
  RDB_ERROR_CHECK(forAccountRole<kDbOperation::kPut>(
      common, account_name, domain_id, default_role));

  // check if account already exists
  if (do_validation)
    RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustNotExist>(
        common, account_name, domain_id));

  common.valueBuffer() = "";
  RDB_ERROR_CHECK(forSignatory<kDbOperation::kPut>(
      common, account_name, domain_id, pubkey));

  common.encode(1);
  RDB_ERROR_CHECK(
      forQuorum<kDbOperation::kPut>(common, account_name, domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::CreateAsset &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &domain_id = command.domainId();
  auto const &asset_name = command.assetName();

  if (do_validation) {
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kCreateAsset));

    // check if asset already exists
    RDB_ERROR_CHECK(forAsset<kDbOperation::kCheck, kDbEntry::kMustNotExist>(
        common, asset_name, domain_id));

    // check if domain exists
    RDB_ERROR_CHECK(forDomain<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, domain_id));
  }

  common.encode(command.precision());
  RDB_ERROR_CHECK(forAsset<kDbOperation::kPut>(common, asset_name, domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::CreateDomain &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &domain_id = command.domainId();
  auto const &default_role = command.userDefaultRole();

  if (do_validation) {
    // no privilege escalation check here
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kCreateDomain));

    // check if domain already exists
    RDB_ERROR_CHECK(forDomain<kDbOperation::kCheck, kDbEntry::kMustNotExist>(
        common, domain_id));

    // check if role exists
    RDB_ERROR_CHECK(forRole<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, default_role));
  }

  uint64_t domains_count = 0ull;
  if (auto result =
          forDomainsTotalCount<kDbOperation::kGet, kDbEntry::kCanExist>(common);
      expected::hasValue(result) && result.assumeValue())
    domains_count = *result.assumeValue();

  common.encode(domains_count + 1ull);
  forDomainsTotalCount<kDbOperation::kPut>(common);

  common.valueBuffer().assign(default_role);
  RDB_ERROR_CHECK(forDomain<kDbOperation::kPut>(common, domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::CreateRole &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &role_name = command.roleName();
  auto role_permissions = command.rolePermissions();
  if (role_permissions.isSet(Role::kRoot))
    role_permissions.setAll();

  if (do_validation) {
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kCreateRole));

    if (!role_permissions.isSubsetOf(creator_permissions))
      return makeError<void>(ErrorCodes::kNoPermissions,
                             "Insufficient permissions");
  }

  // check if role already exists
  if (auto result = forRole<kDbOperation::kCheck, kDbEntry::kMustNotExist>(
          common, role_name);
      expected::hasError(result))
    return makeError<void>(ErrorCodes::kRoleAlreadyExists, "Already exists.");

  common.valueBuffer().assign(role_permissions.toBitstring());
  RDB_ERROR_CHECK(forRole<kDbOperation::kPut>(common, role_name));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::DetachRole &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());
  auto const &role_name = command.roleName();

  if (do_validation)
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kDetachRole));

  RDB_ERROR_CHECK(
      forRole<kDbOperation::kCheck, kDbEntry::kMustExist>(common, role_name));

  if (do_validation)
    RDB_ERROR_CHECK(forAccountRole<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, account_name, domain_id, role_name));

  RDB_ERROR_CHECK(forAccountRole<kDbOperation::kDel>(
      common, account_name, domain_id, role_name));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::GrantPermission &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[grantee_account_name, grantee_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());

  auto const granted_perm = command.permissionName();
  auto const required_perm =
      shared_model::interface::permissions::permissionFor(granted_perm);

  if (do_validation) {
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, required_perm));

    // check if account exists
    RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, account_name, domain_id));
  }

  GrantablePermissionSet granted_account_permissions;
  RDB_TRY_GET_VALUE(
      opt_permissions,
      forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
          common,
          account_name,
          domain_id,
          grantee_account_name,
          grantee_domain_id));
  if (opt_permissions)
    granted_account_permissions = *opt_permissions;

  // check if already granted
  if (granted_account_permissions.isSet(granted_perm))
    return makeError<void>(ErrorCodes::kPermissionIsAlreadySet,
                           "Permission is already set.");

  granted_account_permissions.set(granted_perm);
  common.valueBuffer().assign(granted_account_permissions.toBitstring());
  RDB_ERROR_CHECK(
      forGrantablePermissions<kDbOperation::kPut, kDbEntry::kMustExist>(
          common,
          account_name,
          domain_id,
          grantee_account_name,
          grantee_domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::RemovePeer &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  if (command.pubkey().empty())
    return makeError<void>(ErrorCodes::kPublicKeyIsEmpty, "Pubkey empty.");

  if (do_validation)
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kRemovePeer));

  RDB_ERROR_CHECK(forPeerAddress<kDbOperation::kCheck, kDbEntry::kMustExist>(
      common, command.pubkey()));

  RDB_TRY_GET_VALUE(
      opt_peers_count,
      forPeersCount<kDbOperation::kGet, kDbEntry::kMustExist>(common));
  if (*opt_peers_count == 1ull)
    return makeError<void>(ErrorCodes::kPeersCountIsNotEnough,
                           "Can not remove last peer {}.",
                           command.pubkey());

  common.encode(*opt_peers_count - 1ull);
  RDB_ERROR_CHECK(forPeersCount<kDbOperation::kPut>(common));

  RDB_ERROR_CHECK(forPeerAddress<kDbOperation::kDel>(common, command.pubkey()));
  RDB_ERROR_CHECK(forPeerTLS<kDbOperation::kDel>(common, command.pubkey()));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::RemoveSignatory &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());

  if (do_validation) {
    uint64_t quorum;
    if (auto result = forQuorum<kDbOperation::kGet, kDbEntry::kMustExist>(
            common, account_name, domain_id);
        expected::hasError(result))
      return makeError<void>(ErrorCodes::kNoAccount,
                             std::move(result.assumeError()));
    else
      quorum = *result.assumeValue();

    GrantablePermissionSet granted_account_permissions;
    RDB_TRY_GET_VALUE(
        opt_permissions,
        forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
            common,
            creator_account_name,
            creator_domain_id,
            account_name,
            domain_id));
    if (opt_permissions)
      granted_account_permissions = *opt_permissions;

    if (creator_account_id == command.accountId()) {
      RDB_ERROR_CHECK(
          checkPermissions(creator_permissions, Role::kRemoveSignatory));
    } else {
      RDB_ERROR_CHECK(checkGrantablePermissions(creator_permissions,
                                                granted_account_permissions,
                                                Grantable::kRemoveMySignatory));
    }

    if (auto result = forSignatory<kDbOperation::kCheck, kDbEntry::kMustExist>(
            common, account_name, domain_id, command.pubkey());
        expected::hasError(result))
      return makeError<void>(ErrorCodes::kNoSignatory,
                             std::move(result.assumeError()));

    uint64_t counter = 0;
    auto status = enumerateKeys(common,
                                [&](auto key) {
                                  ++counter;
                                  return true;
                                },
                                fmtstrings::kPathSignatories,
                                domain_id,
                                account_name);
    if (counter <= quorum)
      return makeError<void>(
          ErrorCodes::kCountNotEnough,
          "Remove signatory {} for account {} with quorum {} failed.",
          command.pubkey(),
          command.accountId(),
          quorum);
  }

  RDB_ERROR_CHECK(forSignatory<kDbOperation::kDel>(
      common, account_name, domain_id, command.pubkey()));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::RevokePermission &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[grantee_account_name, grantee_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());

  auto const revoked_perm = command.permissionName();
  auto const required_perm =
      shared_model::interface::permissions::permissionFor(revoked_perm);

  if (do_validation) {
    RDB_ERROR_CHECK(checkPermissions(creator_permissions, required_perm));

    // check if account exists
    RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, account_name, domain_id));
  }

  GrantablePermissionSet granted_account_permissions;
  RDB_TRY_GET_VALUE(
      opt_permissions,
      forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
          common,
          account_name,
          domain_id,
          grantee_account_name,
          grantee_domain_id));
  if (opt_permissions)
    granted_account_permissions = *opt_permissions;

  // check if not granted
  if (!granted_account_permissions.isSet(revoked_perm))
    return makeError<void>(ErrorCodes::kNoPermissions, "Permission not set");

  granted_account_permissions.unset(revoked_perm);
  common.valueBuffer().assign(granted_account_permissions.toBitstring());
  RDB_ERROR_CHECK(
      forGrantablePermissions<kDbOperation::kPut, kDbEntry::kMustExist>(
          common,
          account_name,
          domain_id,
          grantee_account_name,
          grantee_domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::SetAccountDetail &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());

  if (do_validation) {
    if (command.accountId() != creator_account_id) {
      GrantablePermissionSet granted_account_permissions;
      RDB_TRY_GET_VALUE(
          opt_permissions,
          forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
              common,
              creator_account_name,
              creator_domain_id,
              account_name,
              domain_id));
      if (opt_permissions)
        granted_account_permissions = *opt_permissions;

      RDB_ERROR_CHECK(checkPermissions(creator_permissions,
                                       granted_account_permissions,
                                       Role::kSetDetail,
                                       Grantable::kSetMyAccountDetail));
    }

    // check if account exists
    RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, account_name, domain_id));
  }

  RDB_TRY_GET_VALUE(
      opt_detail,
      forAccountDetail<kDbOperation::kGet, kDbEntry::kCanExist>(
          common,
          account_name,
          domain_id,
          !creator_account_id.empty() ? creator_account_id : "genesis",
          command.key()));

  common.valueBuffer().assign(command.value());
  RDB_ERROR_CHECK(forAccountDetail<kDbOperation::kPut>(
      common,
      account_name,
      domain_id,
      !creator_account_id.empty() ? creator_account_id : "genesis",
      command.key()));

  if (!opt_detail) {
    RDB_TRY_GET_VALUE(
        opt_acc_details_count,
        forAccountDetailsCount<kDbOperation::kGet, kDbEntry::kCanExist>(
            common, account_name, domain_id));
    const uint64_t count =
        opt_acc_details_count ? *opt_acc_details_count : 0ull;

    common.encode(count + 1ull);
    RDB_ERROR_CHECK(forAccountDetailsCount<kDbOperation::kPut>(
        common, account_name, domain_id));
  }

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::SetQuorum &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[account_name, domain_id] = staticSplitId<2>(command.accountId());

  if (do_validation) {
    // check if account exists
    RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, account_name, domain_id));

    GrantablePermissionSet granted_account_permissions;
    RDB_TRY_GET_VALUE(
        opt_permissions,
        forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
            common,
            creator_account_name,
            creator_domain_id,
            account_name,
            domain_id));

    if (opt_permissions)
      granted_account_permissions = *opt_permissions;

    RDB_ERROR_CHECK(checkPermissions(creator_permissions,
                                     granted_account_permissions,
                                     Role::kSetQuorum,
                                     Grantable::kSetMyQuorum));
  }

  /// TODO(iceseer): check if is better to store addition value with counter
  int counter = 0;
  auto status = enumerateKeys(common,
                              [&](auto key) {
                                ++counter;
                                return true;
                              },
                              fmtstrings::kPathSignatories,
                              domain_id,
                              account_name);

  if (command.newQuorum() > counter)
    return makeError<void>(ErrorCodes::kCountNotEnough,
                           "Quorum value more than signatories. {}",
                           command.toString());

  common.encode(command.newQuorum());
  RDB_ERROR_CHECK(
      forQuorum<kDbOperation::kPut>(common, account_name, domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::SubtractAssetQuantity &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  // TODO(iceseer): fix the case there will be no delimiter
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[asset_name, domain_id] = staticSplitId<2>(command.assetId());
  auto const &amount = command.amount();

  if (do_validation)
    RDB_ERROR_CHECK(checkPermissions(domain_id,
                                     creator_domain_id,
                                     creator_permissions,
                                     Role::kSubtractAssetQty,
                                     Role::kSubtractDomainAssetQty));

  // check if asset exists
  RDB_TRY_GET_VALUE(opt_result,
                    forAsset<kDbOperation::kGet, kDbEntry::kMustExist>(
                        common, asset_name, domain_id));

  if (*opt_result < command.amount().precision())
    return makeError<void>(
        3,
        "Invalid precision of asset: {} from: {}. Expected: {}, but got: {}",
        command.assetId(),
        creator_account_id,
        *opt_result,
        command.amount().precision());

  shared_model::interface::Amount result(*opt_result);
  RDB_TRY_GET_VALUE(
      opt_amount,
      forAccountAsset<kDbOperation::kGet, kDbEntry::kCanExist>(
          common, creator_account_name, creator_domain_id, command.assetId()));
  if (opt_amount)
    result = std::move(*opt_amount);

  result -= amount;
  common.valueBuffer().assign(result.toStringRepr());
  if (common.valueBuffer()[0] == 'N')
    return makeError<void>(ErrorCodes::kInvalidAmount,
                           "Invalid {} amount {} from {}",
                           command.toString(),
                           result.toString(),
                           creator_account_id);

  RDB_ERROR_CHECK(forAccountAsset<kDbOperation::kPut>(
      common, creator_account_name, creator_domain_id, command.assetId()));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::TransferAsset &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &tx_hash,
    shared_model::interface::types::CommandIndexType /*cmd_index*/,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &[creator_account_name, creator_domain_id] =
      staticSplitId<2>(creator_account_id);
  auto const &[source_account_name, source_domain_id] =
      staticSplitId<2>(command.srcAccountId());
  auto const &[destination_account_name, destination_domain_id] =
      staticSplitId<2>(command.destAccountId());
  auto const &[asset_name, domain_id] = staticSplitId<2>(command.assetId());
  auto const &amount = command.amount();
  auto const &description = command.description();

  // check if destination account exists
  RDB_ERROR_CHECK(forAccount<kDbOperation::kGet, kDbEntry::kMustExist>(
      common, destination_account_name, destination_domain_id));

  // check if source account exists
  RDB_ERROR_CHECK(forAccount<kDbOperation::kCheck, kDbEntry::kMustExist>(
      common, source_account_name, source_domain_id));

  if (do_validation) {
    // get account permissions
    RDB_TRY_GET_VALUE(
        destination_permissions,
        accountPermissions(
            common, destination_account_name, destination_domain_id));
    if (!destination_permissions.isSet(Role::kReceive))
      return makeError<void>(ErrorCodes::kNoPermissions,
                             "Not enough permissions. {}",
                             command.toString());

    if (command.srcAccountId() != creator_account_id) {
      GrantablePermissionSet granted_account_permissions;
      RDB_TRY_GET_VALUE(
          opt_permissions,
          forGrantablePermissions<kDbOperation::kGet, kDbEntry::kCanExist>(
              common,
              source_account_name,
              source_domain_id,
              creator_account_name,
              creator_domain_id));

      if (opt_permissions)
        granted_account_permissions = *opt_permissions;

      RDB_ERROR_CHECK(checkPermissions(creator_permissions,
                                       granted_account_permissions,
                                       Role::kTransfer,
                                       Grantable::kTransferMyAssets));
    } else
      RDB_ERROR_CHECK(checkPermissions(creator_permissions, Role::kTransfer));

    // check if asset exists
    RDB_ERROR_CHECK(forAsset<kDbOperation::kCheck, kDbEntry::kMustExist>(
        common, asset_name, domain_id));

    auto status = common.get(fmtstrings::kSetting,
                             iroha::ametsuchi::kMaxDescriptionSizeKey);
    RDB_ERROR_CHECK(canExist(
        status, [&] { return fmt::format("Max description size key"); }));

    if (status.ok()) {
      uint64_t max_description_size;
      common.decode(max_description_size);
      if (description.size() > max_description_size)
        return makeError<void>(ErrorCodes::kInvalidFieldSize,
                               "Too big description");
    }
  }

  RDB_TRY_GET_VALUE(
      opt_source_balance,
      forAccountAsset<kDbOperation::kGet, kDbEntry::kMustExist>(
          common, source_account_name, source_domain_id, command.assetId()));
  shared_model::interface::Amount source_balance(
      std::move(*opt_source_balance));

  source_balance -= amount;
  if (source_balance.toStringRepr()[0] == 'N')
    return makeError<void>(ErrorCodes::kNotEnoughAssets, "Not enough assets");

  RDB_TRY_GET_VALUE(
      opt_account_asset_size,
      forAccountAssetSize<kDbOperation::kGet, kDbEntry::kCanExist>(
          common, destination_account_name, destination_domain_id));
  uint64_t account_asset_size =
      opt_account_asset_size ? *opt_account_asset_size : 0ull;

  shared_model::interface::Amount destination_balance(
      source_balance.precision());

  RDB_TRY_GET_VALUE(opt_amount,
                    forAccountAsset<kDbOperation::kGet, kDbEntry::kCanExist>(
                        common,
                        destination_account_name,
                        destination_domain_id,
                        command.assetId()));

  if (opt_amount)
    destination_balance = *opt_amount;
  else
    ++account_asset_size;

  destination_balance += amount;
  if (destination_balance.toStringRepr()[0] == 'N')
    return makeError<void>(ErrorCodes::kIncorrectBalance, "Incorrect balance");

  common.valueBuffer().assign(source_balance.toStringRepr());
  RDB_ERROR_CHECK(forAccountAsset<kDbOperation::kPut>(
      common, source_account_name, source_domain_id, command.assetId()));

  common.valueBuffer().assign(destination_balance.toStringRepr());
  RDB_ERROR_CHECK(forAccountAsset<kDbOperation::kPut>(common,
                                                      destination_account_name,
                                                      destination_domain_id,
                                                      command.assetId()));

  common.encode(account_asset_size);
  RDB_ERROR_CHECK(forAccountAssetSize<kDbOperation::kPut>(
      common, destination_account_name, destination_domain_id));

  return {};
}

RocksDbCommandExecutor::ExecutionResult RocksDbCommandExecutor::operator()(
    RocksDbCommon &common,
    const shared_model::interface::SetSettingValue &command,
    const shared_model::interface::types::AccountIdType &creator_account_id,
    const std::string &,
    shared_model::interface::types::CommandIndexType,
    bool do_validation,
    shared_model::interface::RolePermissionSet const &creator_permissions) {
  auto const &key = command.key();
  auto const &value = command.value();

  common.valueBuffer().assign(value);
  RDB_ERROR_CHECK(forSettings<kDbOperation::kPut>(common, key));

  return {};
}
