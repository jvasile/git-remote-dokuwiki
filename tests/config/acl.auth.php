# acl.auth.php
# Auto-generated for integration tests
#
# Format: resource user/group permission
# Permission levels: 0=none, 1=read, 2=edit, 4=create, 8=upload, 16=delete

# Admin group has full access everywhere
*               @admin     16

# Default: all users can read everything
*               @user      1

# Limited user can only edit in the 'limited' namespace
limited:*       @user      16
